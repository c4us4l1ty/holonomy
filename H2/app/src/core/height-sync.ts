/**
 * Batched height synchronisation: the frontend's half of the geometry bridge.
 *
 * # The two paths, and why they are separate
 *
 * **Synchronous.** A `ResizeObserver` measures a section and `LocalGeometry` is
 * updated in the same task, with no await anywhere. That is what makes scroll
 * compensation correct at 60 and 120 FPS: the compensation decision reads heights
 * that are already current. If the geometry waited on an IPC round trip, every
 * measurement would land a frame late and the view would visibly ratchet as the user
 * scrolled through unmeasured sections.
 *
 * **Asynchronous.** The authoritative tree lives in Rust. Measured heights are
 * queued and flushed so Rust learns the same numbers, but that is bookkeeping — it
 * does not affect what the user sees, and it is allowed to be late.
 *
 * Conflating them is the bug this module exists to prevent. The instinct to "just
 * send it" on every measurement is what produces 60 IPC calls a second while someone
 * types.
 *
 * # Trailing throttle, not a trailing debounce
 *
 * These are different and the difference is the whole design.
 *
 * A *debounce* waits for quiescence: each new measurement resets the timer, and the
 * flush happens 300ms after the last one. Under continuous layout shift — a long
 * paragraph reflowing as it is typed, a window drag, an image loading — measurements
 * never stop arriving, so a debounced flush **never fires**. The queue grows without
 * bound and Rust's tree drifts arbitrarily far from the truth.
 *
 * A *throttle* fires at most once per interval, at the end of it. Measurements
 * arriving during a window join the batch that window will send. So continuous
 * shifting produces one IPC call per 300ms rather than none, and the batch carries
 * every section that changed in that window.
 *
 * "300ms trailing throttle" in the directive is this, and `test/boot.ts` asserts it:
 * continuous layout shift must produce exactly one call, not zero and not one per
 * measurement.
 *
 * # Absolute heights, not deltas
 *
 * The queue holds each section's current height rather than the change since last
 * time. That makes the queue idempotent: a section measured forty times before the
 * flush appears once, at its final height, and a retried flush after a failure sends
 * the same value. With deltas, a dropped flush would permanently lose that change —
 * the tree would be wrong and nothing would notice until the user scrolled back.
 *
 * # What happens when a flush fails
 *
 * The entries go back on the queue and the next window retries them. Because the
 * values are absolute, that is safe, and because Rust is authoritative, a lost batch
 * self-heals the next time the section is measured. Swallowing the error would leave
 * Rust's total quietly wrong, which is the one failure mode with no symptom.
 */

import type { HeightUpdate } from './boot'

/** How the batch is sent. A function so a test can count calls without Tauri. */
export type HeightTransport = (updates: HeightUpdate[]) => Promise<unknown>

export interface HeightSyncOptions {
  /** Defaults to the bridge command. */
  readonly send?: HeightTransport
  /** Window length in milliseconds. */
  readonly intervalMs?: number
  /** For the flush-on-teardown path and for tests that need it to be immediate. */
  readonly now?: () => number
}

export interface HeightSyncStats {
  /** Flushes sent. */
  sends: number
  /** Sections sent, across all flushes. */
  sent: number
  /** Flushes that failed and were re-queued. */
  failures: number
  /** Entries waiting for the next window. */
  pending: number
  /** Section ids in the last batch, for the test that asserts batching. */
  lastBatch: string[]
}

export class HeightSync {
  private readonly send: HeightTransport
  private readonly intervalMs: number
  private readonly now: () => number

  /**
   * Latest height per section index.
   *
   * Keyed by index, not by id, because that is what `HeightUpdate` carries and
   * because the geometry is index-keyed — resolving an id per entry would be a
   * lookup per measurement for no benefit. The id travels alongside.
   */
  private readonly queue = new Map<number, HeightUpdate>()

  /**
   * The last height accepted for each section.
   *
   * # Why this is not just the queue
   *
   * The queue is drained every window, so on its own it cannot tell "this section
   * changed" from "this section was measured again and is the same". Those are very
   * different events: the `ResizeObserver` fires for the whole mounted window on any
   * layout change, and the scroller re-measures mounted sections on every refresh, so
   * most measurements during a scroll are of sections whose height did not move.
   *
   * Queuing those would put a round trip on the bridge carrying a value Rust already
   * has — `update_measured_height` treats an identical height as a no-op, so it is
   * harmless, but it is traffic for no information and it is paid on the most common
   * event in the system.
   *
   * Kept across windows rather than cleared with the queue, because the question is
   * "has this section's height moved since we last heard about it", and clearing it
   * would make every window's first measurement of every section a send.
   */
  private readonly lastHeight = new Map<number, number>()

  /** When the current window ends. Null when no window is open. */
  private windowEndsAt: number | null = null
  private timer: ReturnType<typeof setTimeout> | null = null
  private inFlight = false
  /** Re-queued while a send was in flight, so the next window picks them up. */
  private readonly stats: HeightSyncStats = {
    sends: 0,
    sent: 0,
    failures: 0,
    pending: 0,
    lastBatch: [],
  }

  constructor(options: HeightSyncOptions = {}) {
    this.send =
      options.send ??
      (async updates => {
        const { syncSectionHeights } = await import('./geometry-bridge.js')
        return syncSectionHeights(updates)
      })
    this.intervalMs = options.intervalMs ?? 300
    this.now = options.now ?? (() => Date.now())
  }

  /**
   * Record a measured height.
   *
   * Called from the `ResizeObserver` handler, on the same task as the local
   * geometry update. It must not await anything: this runs between a layout and the
   * paint that displays it.
   */
  record(index: number, sectionId: string, height: number): boolean {
    if (index < 0) return false
    if (!Number.isFinite(height) || height < 0) {
      // A layout that has not happened reports zero or NaN. `LocalGeometry.measure`
      // rejects those too; accepting one here would send a section's height to
      // nothing and collapse it on the Rust side.
      return false
    }
    if (this.lastHeight.get(index) === height) return false
    this.lastHeight.set(index, height)
    this.queue.set(index, { section_id: sectionId, index, height })
    this.stats.pending = this.queue.size
    this.openWindow()
    return true
  }

  /**
   * Start the window if one is not already open.
   *
   * # Why the deadline is computed from the clock and not just a timer
   *
   * A plain `setTimeout` on every call is a debounce, and a debounce never fires
   * under continuous change. Computing the deadline once and leaving it alone is what
   * makes this a throttle: measurements inside the window join it, measurements
   * outside it start the next one.
   *
   * The clock is consulted rather than assumed because a batch of measurements can
   * arrive after the timer has already fired but before the queue was drained —
   * `flush` sets `windowEndsAt` to null first, so that cannot happen here, but the
   * check costs nothing and the alternative is a class of bug that only appears under
   * load.
   */
  private openWindow(): void {
    if (this.windowEndsAt !== null) return
    this.windowEndsAt = this.now() + this.intervalMs
    this.timer = setTimeout(() => {
      this.timer = null
      this.windowEndsAt = null
      void this.flush()
    }, this.intervalMs)
  }

  /**
   * Queued entries waiting to be sent.
   *
   * A method rather than a getter, for a reason that shows up in the tests:
   * `ok(sync.pendingCount() === 0, ...)` is an assertion, so TypeScript would narrow
   * a getter to the literal `0` and then reject a later `=== 1` as comparing types
   * with no overlap. The narrowing is correct and useless — the count does change
   * between the assertion and the next line — and a method call is never narrowed.
   */
  pendingCount(): number {
    return this.queue.size
  }

  /** Ids currently queued, in index order. Deterministic for tests and logs. */
  pendingIds(): string[] {
    return [...this.queue.values()].sort((a, b) => a.index - b.index).map(u => u.section_id)
  }

  snapshot(): HeightSyncStats {
    return { ...this.stats, pending: this.queue.size, lastBatch: [...this.stats.lastBatch] }
  }

  /**
   * Send everything queued.
   *
   * Public because teardown needs it — a height measured just before the window
   * closed would otherwise be lost, and it is the last one that matters most, being
   * the document's final shape.
   *
   * # The in-flight guard
   *
   * A send can outlive its window. Without this, a slow send overlapping the next
   * window's would drain the queue twice and the second batch would be empty; worse,
   * two concurrent `sync_section_heights` calls could interleave and leave Rust's
   * total describing neither. The queued entries are simply left for the next
   * window instead.
   */
  async flush(): Promise<void> {
    if (this.inFlight || this.queue.size === 0) return
    this.inFlight = true
    const batch = [...this.queue.values()].sort((a, b) => a.index - b.index)
    // Drained before the await, so a measurement taken while this is in flight lands
    // in the next batch rather than being sent twice.
    this.queue.clear()
    this.stats.pending = 0

    try {
      await this.send(batch)
      this.stats.sends++
      this.stats.sent += batch.length
      this.stats.lastBatch = batch.map(u => u.section_id)
    } catch (e) {
      // Absolute heights make this safe to retry: re-queueing sends the same values,
      // and `update_measured_height` treats an identical height as a no-op.
      for (const u of batch) {
        if (!this.queue.has(u.index)) this.queue.set(u.index, u)
      }
      this.stats.pending = this.queue.size
      this.stats.failures++
      console.error(`[height-sync] batch of ${batch.length} failed; re-queued`, e)
      return
    } finally {
      this.inFlight = false
    }

    // Something arrived while the send was in flight, and no window is open for it
    // because `record` found one already. Open one now rather than waiting for the
    // next measurement, which might be never.
    if (this.queue.size > 0) this.openWindow()
  }

  /** Cancel the pending timer and send what is queued. For teardown. */
  async dispose(): Promise<void> {
    if (this.timer !== null) {
      clearTimeout(this.timer)
      this.timer = null
    }
    this.windowEndsAt = null
    await this.flush()
  }
}
