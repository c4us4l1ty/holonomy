/**
 * Fetching section content on demand, and throwing away what arrives too late.
 *
 * # The race
 *
 * Mounting a section means asking Rust for its bytes. That request is a round trip. By
 * the time it comes back the user may have scrolled somewhere else entirely — and the
 * scroller's buffer window is a *rolling* one, so "somewhere else" is the common case
 * during a fling, not the exception.
 *
 * Without a guard, every response applies. The section's content lands in the record, the
 * cache is told it is resident, and the frontend now believes it has content for a section
 * it will never mount again. Worse, if the response lands *after* a later response for
 * the same section — a fast scroll that re-enters the window — the stale one overwrites
 * the fresh one, and the user sees an older version of the section when they arrive at it.
 *
 * # The generation token
 *
 * One counter, incremented every time the buffer window moves. A response carries the
 * generation it was requested under, and is discarded unless that generation is still
 * current.
 *
 * A timestamp would not do. Two requests made in the same millisecond would compare
 * equal, and the scrollbar can do that. A per-section sequence number would work but
 * answers a different question: it would stop a *stale response for one section* landing,
 * and not stop a response for a section that has left the window altogether — which is
 * the case that leaks entries into the cache.
 *
 * # Why discarding is cheap and applying is not
 *
 * A discarded response costs a `decompress` that has already happened. An applied one
 * costs a decompress, a parse, a cache insert, and a chance to be wrong. The asymmetry is
 * the whole argument for the guard: when in doubt, throw it away.
 *
 * # Injected window and transport
 *
 * `isInWindow` is a function rather than a range, because "in the buffer window" is the
 * scroller's answer to give and it already has one. The transport is injected so this is
 * testable without a Tauri host — the required test is precisely that a fling discards
 * in-flight resolutions, which needs to control resolution order.
 */

import { decompress } from 'fzstd'
import type { SectionContent } from './boot'
import type { SectionCache } from './section-cache.js'

/** Fetches one section's compressed content. */
export type SectionFetcher = (sectionId: string) => Promise<SectionContent>

/** Decode one frame. Shared with the boot path so there is one decoder. */
export function decodeSectionContent(c: SectionContent): unknown {
  if (!(c.content_zstd instanceof Uint8Array)) {
    throw new Error(
      `section ${c.id} content arrived as ${
        c.content_zstd === null ? 'null' : typeof c.content_zstd
      }, not a Uint8Array; the payload was re-encoded as JSON somewhere on the path`,
    )
  }
  return JSON.parse(new TextDecoder().decode(decompress(c.content_zstd)))
}

export interface HydratorOptions {
  readonly fetchSection: SectionFetcher
  readonly cache: SectionCache
  /**
   * Whether a section is still worth holding content for.
   *
   * Called with the section id, not an index: the caller knows its own buffer window and
   * the section's index may have moved while the request was in flight, which is the
   * whole problem.
   */
  readonly isInWindow: (sectionId: string) => boolean
  /** Called with the decoded content when a response is applied. */
  readonly onContent?: (sectionId: string, json: unknown) => void
  /** Called when a fetch fails. Reported rather than swallowed: a section that cannot be */
  /** fetched cannot be edited, and silence would present that as "still loading". */
  readonly onError?: (sectionId: string, error: unknown) => void
}

export interface HydratorStats {
  /** Fetches issued. */
  requested: number
  /** Responses applied. */
  applied: number
  /** Responses discarded because the window had moved past them. */
  discardedStale: number
  /** Responses discarded for the section having left the window. */
  discardedEvicted: number
  /** Requests superseded before they resolved. */
  superseded: number
  /** Fetches that failed. */
  failed: number
  /** Fetches served from the cache, needing no round trip. */
  fromCache: number
}

export class SectionHydrator {
  private readonly fetchSection: SectionFetcher
  private readonly cache: SectionCache
  private readonly isInWindow: (sectionId: string) => boolean
  private readonly onContent?: (sectionId: string, json: unknown) => void
  private readonly onError?: (sectionId: string, error: unknown) => void

  /**
   * The buffer window's generation.
   *
   * Bumped by {@link newGeneration} whenever the window moves. A response from an older
   * generation is stale by definition, whatever its section.
   */
  private generation = 0

  /** Requests still in flight, by section. A second request supersedes the first. */
  private readonly inFlight = new Map<string, number>()

  private readonly stats = {
    requested: 0,
    applied: 0,
    discardedStale: 0,
    discardedEvicted: 0,
    superseded: 0,
    failed: 0,
    fromCache: 0,
  }

  constructor(options: HydratorOptions) {
    this.fetchSection = options.fetchSection
    this.cache = options.cache
    this.isInWindow = options.isInWindow
    this.onContent = options.onContent
    this.onError = options.onError
  }

  /**
   * Declare that the buffer window has moved.
   *
   * Called by the scroller on a scroll, not on a section change. That is deliberate: the
   * window is what makes a response irrelevant, so the window is what it is reported on.
   *
   * It does **not** invalidate in-flight requests outright. See the module header: the
   * scroller reports the new window *after* mounting the sections that entered it, so a
   * bump here lands between a mount's fetch and its resolution, and invalidating on the
   * bump discards every section the user is actually looking at.
   */
  newGeneration(): number {
    this.generation++
    return this.generation
  }

  get currentGeneration(): number {
    return this.generation
  }

  /** Requests outstanding. */
  get pendingCount(): number {
    return this.inFlight.size
  }

  snapshot(): HydratorStats & { pending: number; generation: number } {
    return { ...this.stats, pending: this.inFlight.size, generation: this.generation }
  }

  /**
   * Ensure a section's content is available, fetching it if it is not.
   *
   * Returns the content when it was already resident, `null` when a fetch was started.
   * A caller mounting an editor needs to know which of those happened: `null` means wait,
   * and an editor built on `null` would be built on nothing.
   */
  async ensure(sectionId: string): Promise<{ json: unknown } | null> {
    if (this.cache.includes(sectionId)) {
      const hit = this.cache.get(sectionId)
      if (hit) {
        this.stats.fromCache++
        return hit
      }
    }
    void this.fetch(sectionId, this.generation)
    return null
  }

  /**
   * Fetch one section, and apply it only if it is still wanted.
   *
   * # The order the checks run in
   *
   * 1. **Superseded** — a newer request for the same section exists. First because it is
   *    cheapest, and because the newer request will apply anyway.
   * 2. **Left the window** — `isInWindow`, read at resolution time.
   */
  async fetch(sectionId: string, generation: number): Promise<void> {
    this.stats.requested++
    const previous = this.inFlight.get(sectionId)
    if (previous !== undefined) this.stats.superseded++
    this.inFlight.set(sectionId, generation)

    try {
      const content = await this.fetchSection(sectionId)

      // A newer request for this section has landed while we were waiting.
      if (this.inFlight.get(sectionId) !== generation) {
        this.stats.discardedStale++
        return
      }
      this.inFlight.delete(sectionId)

      if (!this.isInWindow(sectionId)) {
        this.stats.discardedEvicted++
        return
      }

      const json = decodeSectionContent(content)
      // A late arrival must not evict something newer. `set` returns the victim, and a
      // victim that is this same section would mean the cache briefly held two states of
      // it — which `SectionCache` cannot do, so this is a no-op guard rather than a
      // correctness requirement. Kept because the invariant is cheap to assert.
      this.cache.set(sectionId, json)
      this.stats.applied++
      this.onContent?.(sectionId, json)
    } catch (e) {
      this.inFlight.delete(sectionId)
      // A failure is reported even when the response was already stale: the caller has to
      // know the section cannot be fetched, and "it went out of the window" does not mean
      // the next attempt will work.
      this.stats.failed++
      this.onError?.(sectionId, e)
    }
  }

  /**
   * Drop any content for a section and forget its in-flight request.
   *
   * For an eviction that is not about capacity — a section destroyed by the registry
   * after its content was committed. The bytes are in SQLite, and keeping them here would
   * let the cache and the store disagree.
   */
  forget(sectionId: string): void {
    this.cache.delete(sectionId)
    this.inFlight.delete(sectionId)
  }
}
