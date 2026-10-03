/**
 * A `GeometryBridge` backed by a local array, for tests and for the browser
 * harness.
 *
 * # Why this exists rather than calling into Rust
 *
 * The real bridge is a Tauri command. But a Tauri command is async and lives in
 * another process, which means the scroller's behaviour cannot be tested without
 * a Tauri window — and the webkit2gtk path cannot run here.
 *
 * This is a faithful reimplementation of the same semantics, not a mock that
 * returns whatever the test wants. The compensation rule in particular is
 * duplicated exactly, and a test that passes against a mock which got the rule
 * wrong would prove nothing about the shipped path. If the Rust rule changes,
 * this must change with it, and `app/test/run.ts` asserts the two agree on the
 * cases that matter.
 *
 * It is also genuinely useful at runtime: the geometry for a *single* document
 * fits in memory trivially (667 sections is 8KB of f64), and keeping it in the
 * renderer avoids a round trip per scroll event.
 */

import type { GeometryBridge } from './scroller.js'

/** The compensation rule, matching `Geometry::scroll_compensation`.
 *
 * Stated as a standalone function so the test can assert this file and the Rust
 * implementation agree, rather than assuming they do.
 */
export function compensateFor(
  index: number,
  delta: number,
  viewportTop: number,
  heights: number[],
  offsets: number[],
): number {
  if (delta === 0) return 0
  const bottom = offsets[index]! + heights[index]!
  // Entirely above the viewport top: the content being looked at moved.
  if (bottom <= viewportTop) return delta
  // Straddles the top, or below it: the content being looked at did not move.
  return 0
}

export class LocalGeometry implements GeometryBridge {
  private heights: number[]
  /** Running prefix sums, rebuilt lazily. */
  private offsets: number[] = []
  private dirty = true

  /** How much each section was estimated, for the drift report. */
  private estimates: number[]

  constructor(estimates: number[]) {
    this.estimates = [...estimates]
    this.heights = [...estimates]
    this.rebuild()
  }

  private rebuild(): void {
    const o: number[] = new Array(this.heights.length + 1)
    o[0] = 0
    for (let i = 0; i < this.heights.length; i++) o[i + 1] = o[i]! + this.heights[i]!
    this.offsets = o
    this.dirty = false
  }

  private ensureOffsets(): void {
    if (this.dirty) this.rebuild()
  }

  totalHeight(): number {
    this.ensureOffsets()
    return this.offsets[this.offsets.length - 1]!
  }

  offsetOf(index: number): number {
    this.ensureOffsets()
    return this.offsets[Math.min(index, this.heights.length)]!
  }

  heightOf(index: number): number {
    return this.heights[index] ?? 0
  }

  sectionAt(y: number): number {
    this.ensureOffsets()
    if (this.heights.length === 0) return 0
    let lo = 0
    let hi = this.heights.length
    while (lo < hi) {
      const mid = (lo + hi) >> 1
      if (this.offsets[mid + 1]! <= y) lo = mid + 1
      else hi = mid
    }
    return Math.min(lo, this.heights.length - 1)
  }

  visibleRange(y: number, viewportHeight: number, overscan: number): [number, number] {
    const n = this.heights.length
    if (n === 0) return [0, 0]
    const first = Math.max(0, this.sectionAt(y) - overscan)
    const last = Math.min(n, this.sectionAt(Math.max(0, y) + viewportHeight) + 1 + overscan)
    return [first, Math.max(first + 1, last)]
  }

  measure(
    index: number,
    height: number,
    viewportTop: number,
  ): { delta: number; compensate: number } {
    this.ensureOffsets()
    if (index < 0 || index >= this.heights.length) return { delta: 0, compensate: 0 }
    // Same validation as the Rust side: a layout that has not happened reports
    // zero or NaN, and treating that as real would collapse the section.
    if (!Number.isFinite(height) || height < 0) return { delta: 0, compensate: 0 }

    const old = this.heights[index]!
    const delta = height - old
    if (delta === 0) return { delta: 0, compensate: 0 }
    this.heights[index] = height
    this.rebuild()
    return {
      delta,
      compensate: compensateFor(index, delta, viewportTop, this.heights, this.offsets),
    }
  }

  /**
   * Every section's current height, in index order.
   *
   * Exists for one caller: rebuilding the geometry after a split or a merge, where
   * the section count changes and every offset after the cut moves.
   *
   * Heights are re-keyed rather than re-estimated. A height is a property of a
   * section's content, not of its position, so the measurements are still valid —
   * dropping them would send the whole document back to estimates and the scrollbar
   * would visibly lurch after every split. Sections past the end of the old array
   * have no height yet and come back as `undefined`, which the caller replaces with
   * an estimate from the new section's metrics.
   */
  snapshotHeights(): Array<number | undefined> {
    return [...this.heights]
  }

  /**
   * Total drift between estimate and reality.
   *
   * The honest measure of how good the height model is: after everything is
   * measured, how far off was the scrollbar. Reported rather than asserted,
   * because it depends on content no spike can predict.
   */
  driftRatio(): number {
    let est = 0
    let real = 0
    for (let i = 0; i < this.heights.length; i++) {
      est += this.estimates[i]!
      real += this.heights[i]!
    }
    return est > 0 ? real / est : 1
  }

  /** Per-index estimate error, for the calibration report. */
  estimateErrors(): Array<{ index: number; estimated: number; actual: number; ratio: number }> {
    return this.heights.map((h, i) => ({
      index: i,
      estimated: this.estimates[i]!,
      actual: h,
      ratio: this.estimates[i]! > 0 ? h / this.estimates[i]! : 1,
    }))
  }
}
