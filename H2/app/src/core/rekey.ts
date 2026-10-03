/**
 * Re-keying measured heights across a structural change to the section list.
 *
 * # The bug this exists to make impossible
 *
 * When a section splits or merges, the section count changes and every offset after
 * the cut moves. The heights themselves are still valid — a height is a property of a
 * section's content, not of its position — so they must be carried across rather than
 * re-estimated, or the scrollbar lurches on every split.
 *
 * The obvious way to carry them is positional:
 *
 * ```ts
 * new LocalGeometry(newIds.map((_, i) => oldHeights[i] ?? 0))
 * ```
 *
 * and it is wrong. After a split at index `k`, the section that was at index `i > k`
 * is now at `i + 1`, so a positional read hands every measurement past the cut to the
 * wrong section. Splitting index 1 of `[A,B,C]` with heights `[100,200,300]`:
 *
 * ```
 * positional : [100, 200, 300, 0]     <- NEW inherits C's height; C collapses to 0
 * by identity: [100, 100, 100, 300]   <- C keeps its own; NEW gets an allocated share
 * ```
 *
 * Note the *total* is identical in both (600). That is why it survived: the scrollbar
 * kept a plausible length while every section above it was positioned against another
 * section's height, which is exactly the "visible viewport jump" a split must not cause.
 *
 * # What a split and a merge do to the arithmetic
 *
 * - **Split** — one measured height now describes two sections. Divide it in
 *   proportion to the characters that went to each side. Proportional, so the total is
 *   preserved exactly and nothing below the seam moves.
 * - **Merge** — two measured heights now describe one section. Add them, for the same
 *   reason.
 *
 * Both need to be told which section was affected. Inferring it from two orderings
 * means guessing which id is new, and a wrong guess reassigns measurements to unrelated
 * sections — so the caller states it. See `GeometryChange` in `./lifecycle.ts`.
 */

import type { GeometryChange } from './lifecycle.js'

export interface RekeyInput {
  /** Which section each entry of `previousHeights` belonged to, in order. */
  previousIds: readonly string[]
  /** The heights as they were, in the same order as `previousIds`. */
  previousHeights: ReadonlyArray<number | undefined>
  /** The new ordering, after the change. */
  newIds: readonly string[]
  /** What the change did, so heights can be divided or summed. */
  change?: GeometryChange
  /**
   * Height for a section with no measurement and no allocation.
   *
   * Called only for sections that have genuinely never been on screen. The product
   * passes the metrics-based estimate; the default is `0` so this function is usable
   * without a calibration, and callers that care supply a real estimate.
   */
  estimate?: (sectionId: string) => number
}

/**
 * Heights for `newIds`, in that order.
 *
 * Pure: it reads no state and returns a fresh array. That is what makes the failure
 * above testable at all — the bug lived inside a closure over module state in `main.ts`,
 * where a unit test could only reach it through a running browser.
 */
export function rekeyHeights(input: RekeyInput): number[] {
  const { previousIds, previousHeights, newIds, change, estimate } = input

  // Identity -> measured height. Ids are unique (they are SQLite primary keys), so a
  // Map is exact, and it turns the per-section lookup into O(1) instead of an
  // `indexOf` scan per section — which on a 2000-section document would be 2000 scans.
  const byId = new Map<string, number>()
  previousIds.forEach((id, i) => {
    const h = previousHeights[i]
    if (h !== undefined) byId.set(id, h)
  })

  if (change?.kind === 'split') {
    const measured = byId.get(change.sectionId)
    if (measured !== undefined) {
      const denom = change.headChars + change.tailChars
      // Proportional to the characters each half received, which is proportional to
      // its rendered height.
      //
      // A zero denominator means neither half holds text worth measuring — two
      // image-only sections, say — so an even split is the only answer that is not
      // arbitrary, and it is what a user would draw by hand.
      const headShare = denom > 0 ? change.headChars / denom : 0.5
      byId.set(change.sectionId, measured * headShare)
      // The tail is a genuinely new section and has no entry of its own, which is why
      // it is found by position *here*: this is the one place the caller has stated
      // which section was divided, so "the one immediately after it" is exact rather
      // than a guess.
      const at = newIds.indexOf(change.sectionId)
      const tailId = at >= 0 ? newIds[at + 1] : undefined
      // `1 - headShare` rather than recomputing from `tailChars`, so the two halves
      // always sum to exactly `measured` even when `chars` disagree with the
      // character totals of the JSON they were derived from.
      if (tailId !== undefined) byId.set(tailId, measured * (1 - headShare))
    }
  } else if (change?.kind === 'merge') {
    const kept = byId.get(change.intoSectionId)
    const gone = byId.get(change.removedId)
    if (kept !== undefined && gone !== undefined) {
      byId.set(change.intoSectionId, kept + gone)
    } else if (kept !== undefined) {
      // The removed section was never measured, so there is nothing to add. But the
      // survivor's own height is now *known* to be too small — it describes the
      // content before the merge, not after — so keeping it would under-report the
      // document by an unknown amount, and every section below would be positioned
      // too high.
      //
      // Dropping the entry lets it fall through to the estimate, which is measured
      // from the merged section's real metrics. An overestimate is the safe direction:
      // it is corrected on the first `measure`, whereas an underestimate persists until
      // the user scrolls to that section.
      byId.delete(change.intoSectionId)
    }
  }

  return newIds.map(id => {
    const known = byId.get(id)
    if (known !== undefined) return known
    // No measurement and no allocation. A fresh section the user has never scrolled
    // to, which is normal after a merge collapses a neighbourhood. Returning 0 would
    // collapse it to nothing and leave a hole in the document the user could scroll
    // into, so an estimate is strictly better than silence here.
    return estimate ? estimate(id) : 0
  })
}