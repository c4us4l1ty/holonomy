/**
 * Persisting edits: the 1000ms trailing throttle, and the flush an eviction owes.
 *
 * # Two paths, for the same reason `height-sync.ts` has two
 *
 * **Typing.** Every keystroke appends to the write-ahead log. Doing that per keystroke
 * would be a round trip and a zstd compression per character, so edits are coalesced
 * into a window and sent at most once per interval. Nothing waits on it: the editor has
 * already applied the change, and the buffer is a durability concern, not a rendering
 * one.
 *
 * **Eviction.** A section about to be destroyed is different. The bytes exist only in
 * the live ProseMirror instance, and after `destroy()` they exist nowhere in the
 * renderer. Waiting for a throttle window there is not "slightly late", it is data loss,
 * because there is no second copy to retry from.
 *
 * So the throttle can be bypassed, and the eviction path does exactly that. See
 * {@link EditCommitter.flushNow}.
 *
 * # Why the payload is the document and not a diff
 *
 * A diff would be smaller and would need a reconstruction path on the other side — a
 * second way for the store and the renderer to disagree, and one that is only exercised
 * when a diff is applied out of order. A section is ~7KB compressed; the WAL row is
 * replaced per section rather than appended, so the cost is one row's worth of writes
 * per window, not one per keystroke.
 *
 * # Trailing throttle, not debounce
 *
 * Same distinction as the height sync, for the same reason: a debounce never fires while
 * the user is typing, which is exactly when it matters. One send per window, with
 * everything in that window coalesced into the last state seen.
 */

import type { CommitResponse } from './boot'

/** What gets sent. Injected so a test can count calls without a Tauri host. */
export type EditTransport = (sectionId: string, json: unknown, markCount: number) => Promise<CommitResponse>

/** Folds the write-ahead log. Separate because it is a different guarantee. */
export type FlushTransport = (documentId?: string) => Promise<number>

/**
 * How a section's current state is read.
 *
 * A function rather than a value because the point of the throttle is that the value
 * changes many times between sends: capturing it when the window opens would persist a
 * state the user has already typed past.
 */
export type ReadSection = () => { json: unknown; markCount: number } | null

export interface EditCommitterOptions {
  readonly send?: EditTransport
  readonly flush?: FlushTransport
  /** Default 1000ms, per the directive. */
  readonly intervalMs?: number
  readonly now?: () => number
}

export interface EditCommitterStats {
  sends: number
  /** Sections sent, across all sends. One per window per section. */
  sectionsSent: number
  /** Immediate sends, from an eviction. */
  evictions: number
  /** Sends that failed and were left for the next window. */
  failures: number
  /** Sections with an unsent edit right now. */
  pending: number
  /** Sections whose latest send failed. */
  failed: number
}

export class EditCommitter {
  private readonly send: EditTransport
  private readonly flushFn: FlushTransport | null
  private readonly intervalMs: number
  private readonly now: () => number

  /**
   * Sections with an edit not yet sent, in the order they were first touched.
   *
   * Ordered so a window's batch is deterministic, which matters for a log and for a test
   * that asserts what went out.
   */
  private readonly pendingIds: string[] = []
  private readonly readers = new Map<string, ReadSection>()
  /** Latest mark count seen per section, for the retry path. */
  private readonly markCounts = new Map<string, number>()
  private readonly failedIds = new Set<string>()

  private windowEndsAt: number | null = null
  private timer: ReturnType<typeof setTimeout> | null = null
  private inFlight = false
  private readonly stats: EditCommitterStats = {
    sends: 0,
    sectionsSent: 0,
    evictions: 0,
    failures: 0,
    pending: 0,
    failed: 0,
  }

  constructor(options: EditCommitterOptions = {}) {
    this.send =
      options.send ??
      (async (sectionId, json, markCount) => {
        const { commitSectionEdit } = await import('./geometry-bridge.js')
        return commitSectionEdit(sectionId, json, markCount)
      })
    this.flushFn =
      options.flush ??
      (async documentId => {
        const { flushDocument } = await import('./geometry-bridge.js')
        return flushDocument(documentId)
      })
    this.intervalMs = options.intervalMs ?? 1000
    this.now = options.now ?? (() => Date.now())
  }

  /**
   * Note that a section has changed.
   *
   * Called from the registry's change handler, which runs on the keystroke path — so
   * this must not await, allocate a payload, or touch the store. It records where to read
   * from and opens a window.
   */
  record(sectionId: string, read: ReadSection): void {
    if (!this.readers.has(sectionId)) this.pendingIds.push(sectionId)
    this.readers.set(sectionId, read)
    this.stats.pending = this.pendingIds.length
    this.openWindow()
  }

  private openWindow(): void {
    if (this.windowEndsAt !== null) return
    this.windowEndsAt = this.now() + this.intervalMs
    this.timer = setTimeout(() => {
      this.timer = null
      this.windowEndsAt = null
      void this.flush()
    }, this.intervalMs)
  }

  pending(): number {
    return this.pendingIds.length
  }

  pendingSectionIds(): string[] {
    return [...this.pendingIds]
  }

  snapshot(): EditCommitterStats {
    return { ...this.stats, pending: this.pendingIds.length, failed: this.failedIds.size }
  }

  /**
   * Send everything pending.
   *
   * Each section is read at send time rather than at record time, so what goes out is
   * the state the user last had rather than the state they had when the window opened.
   */
  async flush(): Promise<void> {
    if (this.inFlight || this.pendingIds.length === 0) return
    this.inFlight = true
    const batch = [...this.pendingIds]
    this.pendingIds.length = 0
    this.stats.pending = 0

    try {
      for (const sectionId of batch) {
        const read = this.readers.get(sectionId)
        if (!read) continue
        const state = read()
        // No reader and no section means the editor went away before the window closed.
        // That is the eviction path's job, not this one's: it has already been flushed by
        // then, or the section was never dirty.
        if (!state) {
          this.readers.delete(sectionId)
          continue
        }
        this.markCounts.set(sectionId, state.markCount)
        await this.send(sectionId, state.json, state.markCount)
        // Cleared only after the send succeeds. The first version deleted the reader
        // before sending, so a failed batch left its sections in the queue with nothing
        // left to read them: the retry found no reader, skipped them, and the queue
        // emptied without ever having persisted the edits.
        //
        // The difference is invisible in the passing case and is exactly the difference
        // between a retry and data loss, which is why the test asserts the attempt count
        // rather than only the final state.
        this.readers.delete(sectionId)
        this.failedIds.delete(sectionId)
        this.stats.sectionsSent++
      }
      this.stats.sends++
    } catch (e) {
      // The section that failed goes back at the *front*, so ordering is preserved for
      // the next window and the ones that never went are not reordered behind new work.
      for (const id of [...batch].reverse()) {
        if (!this.pendingIds.includes(id)) this.pendingIds.unshift(id)
      }
      this.failedIds.add(batch[0]!)
      this.stats.failed = this.failedIds.size
      this.stats.failures++
      console.error(`[edit-commit] batch of ${batch.length} failed; re-queued`, e)
    } finally {
      this.inFlight = false
    }

    if (this.pendingIds.length > 0) this.openWindow()
  }

  /**
   * Send one section now and fold the log, because its editor is about to be destroyed.
   *
   * # Why this bypasses the throttle
   *
   * Because after `destroy()` the only copy of these bytes is in SQLite. The throttle is
   * a latency optimisation for a section that still has its editor; here the editor is
   * the thing at risk.
   *
   * # Why it folds as well as sending
   *
   * `commit_section_edit` makes the edit durable in the recovery buffer, and the buffer
   * is folded into the section row on the next open. So without a fold, an evicted
   * section is only as safe as the next checkpoint — which is the next app launch, and
   * a crash before then loses it. Folding here means "this section is in its row", which
   * is the property a reader would assume a saved edit has.
   *
   * # Why the fold is best-effort
   *
   * A fold failure must not throw out of an unmount: the editor is being destroyed either
   * way, and an exception here would leave the caller with no cleanup at all. The commit
   * has already been logged, which is the durable part, so a failure here costs the
   * in-row guarantee rather than the edit.
   *
   * Returns whether the section was dirty. `false` means there was nothing to send, and
   * the caller can skip the flush entirely — which is what keeps a scroll through a
   * document nobody edited from folding on every section.
   */
  async flushNow(sectionId: string, documentId?: string): Promise<boolean> {
    const read = this.readers.get(sectionId)
    // A pending window entry for a section with no reader is already accounted for: the
    // editor is gone, so there is nothing to read and nothing to send.
    if (!read) return false
    const state = read()
    this.readers.delete(sectionId)
    const at = this.pendingIds.indexOf(sectionId)
    if (at >= 0) this.pendingIds.splice(at, 1)
    this.stats.pending = this.pendingIds.length
    if (!state) return false

    try {
      await this.send(sectionId, state.json, state.markCount)
      this.failedIds.delete(sectionId)
      this.stats.evictions++
      this.stats.sectionsSent++
    } catch (e) {
      console.error(`[edit-commit] eviction flush of ${sectionId} failed`, e)
      this.failedIds.add(sectionId)
      this.stats.failed = this.failedIds.size
      this.stats.failures++
      return false
    }

    if (this.flushFn) {
      try {
        await this.flushFn(documentId)
      } catch (e) {
        console.error('[edit-commit] fold after eviction failed; the edit is in the log', e)
      }
    }
    return true
  }

  /** Whether a section has an unsent edit. */
  isPending(sectionId: string): boolean {
    return this.readers.has(sectionId)
  }

  /** Cancel the timer and send what is queued. For teardown. */
  async dispose(): Promise<void> {
    if (this.timer !== null) {
      clearTimeout(this.timer)
      this.timer = null
    }
    this.windowEndsAt = null
    await this.flush()
  }
}
