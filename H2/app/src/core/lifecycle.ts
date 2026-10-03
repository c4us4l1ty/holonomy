/**
 * Section lifecycle: deciding when to split, finding a cut point, and applying the
 * result without disturbing the user.
 *
 * # The three problems, which are not the same problem
 *
 * 1. **Deciding.** A section over 1500 words or 3000 marks should split. Those
 *    limits come from measurement (M0's rendering knee, M1b's mark ceiling), not from
 *    taste, and they live in `holonomy_core::split` so Rust and the frontend cannot
 *    disagree about them.
 * 2. **Cutting.** Somewhere sensible, not just somewhere valid. A cut inside a table
 *    or immediately after a heading produces two sections that are each worse to
 *    edit than the section they replaced.
 * 3. **Surviving.** The user must not notice. Focus stays in the section they were
 *    typing in, the caret keeps its place, and the scroll position does not jump.
 *
 * The third is where this could easily have gone wrong, and it is why reconciliation
 * is a separate step from the request.
 *
 * # Why the request and the reconciliation are separate
 *
 * `commit_section_lifecycle` returns the whole new section ordering, not a diff,
 * because a split moves order keys on both sides of the cut and a patch would have to
 * be exactly right about what did *not* change. Reconciling against the full ordering
 * is one operation with no partial-failure case.
 *
 * What the reply cannot tell us is what happened to *our* editor. The frontend had a
 * live ProseMirror instance with a caret in it; Rust has two rows and a content
 * split. So the frontend's job after the reply is to rebuild what it lost from what
 * it already had, and that is a different concern from persisting the split.
 *
 * # Why the caret is found from the live document, not from a saved position
 *
 * A section's content changes underneath a live editor. Any position captured before
 * the split is a position in a document that no longer exists. So the caret is
 * recorded as a *character offset from the start of the section* — a quantity that
 * survives a split, because the split only divides the content and does not reorder
 * or rewrite it — and re-resolved against whichever half now contains it.
 */

import type { LifecycleAction, LifecycleResult } from './boot'
import type { SectionRecord, SectionRegistry } from './registry.js'

/**
 * The section limits.
 *
 * Mirrors `holonomy_core::split::{MAX_WORDS_PER_SECTION, MAX_MARKS_PER_SECTION}`.
 * Duplicated as constants rather than imported because the frontend has no access to
 * the Rust values, and `test/lifecycle.ts` asserts these match the Rust source. That
 * is a parity test between two hand-written copies, which is normally the thing this
 * project avoids — but the alternative is a fourth bridge command whose only job is
 * to deliver two integers, and a number delivered over IPC cannot be read before the
 * first keystroke anyway.
 *
 * The alternative worth considering is putting the limits in the boot payload. It is
 * better, and it is listed in `STATUS.md`; it is not done here because it changes the
 * contract for a threshold that has been correct since M1b and would need the
 * cross-engine suite re-run for no behavioural gain.
 */
export const MAX_WORDS_PER_SECTION = 1500
export const MAX_MARKS_PER_SECTION = 3000

/** Why a section should be split. */
export type SplitTrigger = 'words' | 'marks' | null

/** Which limit, if any, this section has passed. */
export function splitTrigger(
  metrics: { words: number; marks: number },
  limits: { words?: number; marks?: number } = {},
): SplitTrigger {
  const maxWords = limits.words ?? MAX_WORDS_PER_SECTION
  const maxMarks = limits.marks ?? MAX_MARKS_PER_SECTION
  if (metrics.words > maxWords) return 'words'
  if (metrics.marks > maxMarks) return 'marks'
  return null
}

/**
 * Blocks that are over budget *on their own* and cannot be divided.
 *
 * # Why this needs to exist
 *
 * `splitTrigger` asks "is this section over its limit?" and `chooseCutIndex` asks "is there a
 * legal seam?". Neither notices that a section can be over its limit for a reason no seam can
 * fix: one table with 2,500 words in it. A table is atomic, so it is the whole of one section
 * or none of it, and every legal seam is either outside the table (leaving a section that is
 * still over budget) or inside it (which is not legal).
 *
 * The result is a section that stays over its limit forever, which is fine — that is what
 * atomic means — but the coordinator was asking on *every keystroke* and being told yes, and
 * the answer it wants is the same one every time. So this reports the blocks, and the caller
 * can log once rather than log continuously.
 *
 * # What it does not do
 *
 * It does not refuse to split. A section of `paragraph, table(2500 words), paragraph` can still
 * be divided at the paragraph boundaries, and should be: that is two over-budget sections
 * instead of one, and the seam is real. This reports the *block*, not the section.
 *
 * # The word count is the same measure as everywhere else
 *
 * `blockWeight`, summed through nesting, which is a proxy for rendered height and not a word
 * count. Used here as "is this block plausibly too big on its own", which is the only question
 * being asked. `blockWeight` is what `chooseCutIndex` already balances against, so a block this
 * calls oversized is one the splitter already knows it cannot make smaller.
 */
export function unsplittableBlocks(
  json: unknown,
  limits: { words?: number } = {},
): Array<{ index: number; type: string; weight: number }> {
  const maxWords = limits.words ?? MAX_WORDS_PER_SECTION
  const nodes = topLevelNodes(json)
  const weights = blockWeights(nodes)
  const out: Array<{ index: number; type: string; weight: number }> = []
  for (let i = 0; i < nodes.length; i++) {
    const weight = weights[i]!
    if (weight <= maxWords) continue
    if (!isAtomicBlock(nodes[i])) continue
    out.push({ index: i, type: String((nodes[i] as { type?: string }).type ?? 'unknown'), weight })
  }
  return out
}

/**
 * The top-level nodes a section's content is made of.
 *
 * Only the top level, because that is what a split divides: the whole point is that
 * a section is a run of blocks, and cutting between blocks is the only cut that does
 * not leave a half-open structure behind.
 */
function topLevelNodes(json: unknown): Array<{ type?: string; isTextblock?: boolean; size?: number }> {
  const content = (json as { content?: unknown[] } | null)?.content
  return Array.isArray(content) ? (content as Array<{ type?: string; isTextblock?: boolean; size?: number }>) : []
}

/**
 * How much room each top-level block takes, as a weight.
 *
 * # Why this walks the JSON rather than reading a size
 *
 * An earlier version read `node.size`, which ProseMirror's `Node` has and ProseMirror's
 * **JSON does not**. So every block fell through to the `?? 1` default, every weight was
 * 1, and the "even by size" search was really "even by block count" — quietly
 * reimplementing the thing the function exists to avoid, and passing a test that only
 * checked the result was a legal boundary. `test/lifecycle.ts` caught it by asking for
 * a specific cut on a document whose weights are wildly uneven.
 *
 * # What the weight is
 *
 * Text length, summed through nesting, with a floor of 1 so a block with no text of its
 * own — an image, a rule, an empty paragraph — still counts. That is a proxy for
 * rendered height and deliberately not a model: the geometry has a fitted one, this is
 * for choosing a seam, and a proxy that is monotone in content is all that needs.
 *
 * The floor matters in the other direction too: without it, a run of empty paragraphs
 * would weigh zero and a cut could be placed "between" several of them at no cost, which
 * is how a section ends up with a hundred blank lines in it.
 */
function blockWeight(node: unknown): number {
  if (!node || typeof node !== 'object') return 0
  const n = node as { text?: string; content?: unknown[] }
  if (typeof n.text === 'string') return n.text.length
  if (!Array.isArray(n.content)) return 1
  return Math.max(1, n.content.reduce<number>((a, c) => a + blockWeight(c), 0))
}

/** One weight per top-level block, in document order. */
function blockWeights(nodes: unknown[]): number[] {
  return nodes.map(blockWeight)
}

/**
 * Pick the block index to cut at, so the two halves are as close to equal as the block
 * boundaries allow.
 *
 * # Even by size, and by nothing else
 *
 * A cut that put the first half at exactly 1500 words regardless of where the blocks
 * fall would be better on paper and worse in practice: it would have to cut *inside* a
 * block, which is the one thing that cannot be done safely. So the cut is snapped to a
 * boundary and the halves are as even as the boundaries allow.
 *
 * # Why a share constraint, and why it replaced a positional window
 *
 * Minimising |head - total/2| alone degenerates when one block dominates. Ten
 * two-character blocks and one 9000-character block has a midpoint no legal boundary is
 * near, so the objective picks whichever boundary is marginally less bad and the result
 * is a two-character first half -- which is not a split, it is a fragment.
 *
 * The first fix here was a positional window: search only the middle 40% of the block
 * *indices*. That fails in the opposite direction and worse, because weight
 * concentration and position are unrelated. Three 1200-character blocks followed by
 * twenty two-character ones has its true midpoint at block 3, and a positional window
 * beginning at block 4 cannot see it: the objective was right and the window threw the
 * answer away. Measured, it cut at 12 where 4 was correct.
 *
 * A *share* constraint states what actually matters -- each half should hold a
 * substantial share of the section.
 *
 * - Prefer boundaries whose head holds between `MIN_SHARE` and `MAX_SHARE` of the
 *   total, and among those the one closest to half.
 * - If none qualifies -- which happens exactly when the weight is too concentrated to
 *   split evenly at all -- take the best legal boundary there is and let the section be
 *   lopsided. A lopsided but legal split beats a fragment and beats an error.
 */
const MIN_SHARE = 0.3
const MAX_SHARE = 0.7

/**
 * Block types that may not start a new section.
 *
 * # Why this list, and what it is actually for
 *
 * Not because cutting *inside* one is possible. It is not: a seam is a top-level block
 * index, so no seam can fall within a table, a code block or an equation. That is a
 * consequence of the representation, not something a list can enforce.
 *
 * It is for the other direction. `SectionRegistry.mergeBackward` -- the Backspace-at-
 * position-0 gesture that undoes a split -- refuses to move anything that is not a
 * textblock:
 *
 * > if (!firstBlock.isTextblock) return false
 *
 * So a seam that leaves an atomic block as the *tail's first child* creates a boundary the
 * user cannot cross back. They can type across it; they cannot press Backspace across it;
 * and nothing explains why. A section that has been split cannot be rejoined by the gesture
 * that exists to rejoin split sections.
 *
 * This list is therefore the set of blocks that make a seam irreversible, and
 * {@link chooseCutIndex} declines to choose such a seam.
 *
 * # Why a list rather than asking the schema
 *
 * Because the check runs on raw JSON, before any editor exists, and ProseMirror's JSON
 * carries no `isTextblock` -- the registry reads that off a live `Node`, which is a
 * different representation of the same fact. Enumerating the types here is a second source
 * of that answer, so `test/lifecycle.ts` asserts the two agree on the block types the
 * editor actually installs: a new atom fails the test rather than silently becoming
 * mergeable or not.
 *
 * # Why `image` and `horizontalRule` are here
 *
 * Both are `atom: true`, and a rule is not a textblock either. A seam in front of a figure
 * leaves a section opening on a figure, which is poor to edit and impossible to Backspace
 * across. There is no reason to treat them differently from a table.
 */
export const ATOMIC_BLOCK_TYPES: ReadonlySet<string> = new Set([
  'table',
  'codeBlock',
  'mathBlock',
  'equation',
  'image',
  'horizontalRule',
])

/** Whether a top-level block is one that may not start a section. */
export function isAtomicBlock(node: unknown): boolean {
  const type = (node as { type?: unknown } | null)?.type
  return typeof type === 'string' && ATOMIC_BLOCK_TYPES.has(type)
}

/**
 * Whether a seam at block `cut` can be crossed back by a merge.
 *
 * Reversible means the tail's first block can be moved into the head by a Backspace at
 * position 0, which is the only way a user re-joins two sections. See
 * {@link ATOMIC_BLOCK_TYPES}.
 */
export function seamIsReversible(nodes: ReadonlyArray<{ type?: string }>, cut: number): boolean {
  const first = nodes[cut]
  if (!first) return false
  return !isAtomicBlock(first)
}

/**
 * The block types the installed editor declares as atoms, for the parity check.
 *
 * Takes a ProseMirror node map (`schema.nodes`), not a schema.
 *
 * # Why this is read out of the schema rather than written down again
 *
 * Because the registry's `!firstBlock.isTextblock` is the rule that actually refuses the
 * merge, and it reads ProseMirror's own `atom` flag. A list in this module that disagreed
 * with that flag would make `chooseCutIndex` decline a seam that is in fact reversible, or
 * accept one that is not. Exposing the schema's answer lets `test/lifecycle.ts` compare the
 * two instead of asking a reader to.
 */
export function atomicTypesFromSchema(nodes: Record<
  string,
  { spec?: { atom?: boolean; group?: string; isolating?: boolean } }
>): string[] {
  return Object.entries(nodes)
    // Top-level blocks only, because only they can be a seam's neighbour. `doc`'s content
    // spec is `block+`, so a node outside the `block` group cannot sit directly under it --
    // and `tableCell` is the case that matters: it is an atom, it has `isolating: true`,
    // and it can only ever appear inside a `tableRow`, so it can never be the first block
    // of a section. Comparing the whole atom set against a list of *block* types failed on it
    // correctly, and the fix is the filter rather than the list.
    .filter(([, n]) => n.spec?.group?.split(' ').includes('block') === true)
    .filter(([, n]) => n.spec?.atom === true || n.spec?.isolating === true)
    .map(([type]) => type)
    .sort()
}

export function chooseCutIndex(json: unknown): number | null {
  const nodes = topLevelNodes(json)
  // Fewer than two blocks means there is no interior boundary. The caller reports a
  // refusal; this returns null rather than a number that would be clamped into producing
  // an empty half.
  if (nodes.length < 2) return null

  const weights = blockWeights(nodes)

  // `before[i]` is the weight preceding block `i`, which is what a cut at `i` leaves in
  // the head. An earlier version accumulated `weights[i]` *before* scoring, so every
  // candidate was scored one block too heavy.
  const before: number[] = new Array(nodes.length)
  let running = 0
  for (let i = 0; i < nodes.length; i++) {
    before[i] = running
    running += weights[i]!
  }
  const total = running
  if (total <= 0) return Math.floor(nodes.length / 2)

  const legal: number[] = []
  for (let i = 1; i < nodes.length; i++) legal.push(i)

  // The irreversible seams come out first, not last. Every interior boundary is a valid
  // *position* -- the representation guarantees that -- but some of them cannot be crossed
  // back, and a boundary the user cannot undo is worse than a lopsided section. So:
  //
  // 1. reversible seams with a reasonable share,
  // 2. reversible seams whatever the share,
  // 3. refuse.
  //
  // There is deliberately no fourth option of "any seam, lopsided or not". A section over
  // its limit that cannot be split cleanly is a real cost, and it is the smaller one: it
  // grows a little further and tries again, rather than splitting into a pair where one of
  // them can never be put back together. The third case is reached only when the head is
  // followed immediately by an atom, which is rare and visible.
  const reversible = legal.filter(i => seamIsReversible(nodes, i))
  // The 2,500-word table is refused *here*, not by a rule about being over budget. Every legal
  // seam puts an atom first in the tail, so `reversible` is empty.
  //
  // There was a guard for "every block is over budget on its own" and it was dead: a section of
  // only over-budget *atoms* is already refused by the line above, and a section of over-budget
  // *prose* must be split — cutting it produces two smaller sections that a later keystroke can
  // divide again, which is the whole point of having a word limit. The mutation run caught it:
  // removing the guard changed no test result, which is the definition of unreachable.
  if (reversible.length === 0) return null

  const eligible = reversible.filter(i => {
    const share = before[i]! / total
    return share >= MIN_SHARE && share <= MAX_SHARE
  })

  const closest = (candidates: number[]): number => {
    let best = candidates[0]!
    let bestError = Math.abs(before[best]! - total / 2)
    for (const i of candidates) {
      const error = Math.abs(before[i]! - total / 2)
      if (error < bestError) {
        bestError = error
        best = i
      }
    }
    return best
  }

  const chosen = closest(eligible.length > 0 ? eligible : reversible)

  // A postcondition rather than a hope. `cut` is a top-level index and the whole design
  // rests on that: it is why no seam can fall inside a table, and it is why `applySplit`'s
  // `nodes.slice(0, cut)` produces two documents the editor can mount. A future change that
  // made cuts position- or byte-based would be caught here -- at the point of the change,
  // with the block types in hand -- rather than by a test someone has to notice and
  // rewrite.
  if (!seamIsReversible(nodes, chosen)) {
    throw new Error(
      `chooseCutIndex chose block ${chosen}, whose tail starts with ` +
        `${nodes[chosen]?.type ?? 'an unknown block'}; that seam cannot be crossed back by a merge`,
    )
  }
  return chosen
}


/**
 * What a structural change did to the sections' content, in the terms the geometry
 * needs to re-derive heights.
 *
 * # Why this has to be said rather than inferred
 *
 * The caller has a list of section ids before and after, and from those alone it cannot
 * tell a split from a merge — both change the count by one. But the two need opposite
 * height arithmetic:
 *
 * - a **split** divides one measured height between two sections, in proportion to the
 *   characters that went to each side, so the total is preserved exactly and the
 *   scrollbar does not move;
 * - a **merge** adds one section's height to another's, for the same reason.
 *
 * Inferring this from the id lists means guessing which id is "new" by comparing two
 * orderings, and a guess that is wrong reassigns measured heights to unrelated sections.
 * That is not hypothetical: the version of `reindex` that did exactly this mapped
 * heights by *position*, so after a split at index 1 of `[A,B,C]` the new section
 * inherited `C`'s measured height and `C` itself fell to `0` — every section past the
 * cut was given its successor's height, while the total stayed correct enough that
 * nothing looked wrong.
 */
export type GeometryChange =
  | {
      kind: 'split'
      /** The section that was divided. It survives as the head. */
      sectionId: string
      /** Non-whitespace characters in the head. The denominator is their sum. */
      headChars: number
      /** Non-whitespace characters in the tail that became the new section. */
      tailChars: number
    }
  | {
      kind: 'merge'
      /** The surviving section, which absorbed the other's content. */
      intoSectionId: string
      /** The section that was removed. */
      removedId: string
    }

/**
 * What the caller supplies, so this module holds no DOM and no editor state.
 *
 * The coordinator decides *what* to do and this interface is how it says so. Everything
 * stateful — the registry, the geometry, the transport — belongs to the caller, which is
 * what lets every function here be tested against a fake with no Tauri host and no
 * rendering engine.
 */
export interface LifecycleHost {
  /** The section registry: records, ordering, editors. */
  readonly registry: SectionRegistry
  /** Section ids in document order, which is what a `LifecycleResult` returns. */
  sectionIds(): string[]
  /** Send a lifecycle action to the store. */
  commit(action: LifecycleAction): Promise<LifecycleResult>
  /**
   * Rebuild the geometry for a new ordering.
   *
   * The section count changed, so every offset after the change point moved. Heights
   * must be re-keyed **by section identity**, not by position: a height is a property
   * of a section's content, and after a split the sections at indices past the cut are
   * all shifted by one.
   *
   * `change` describes what moved, so the host can allocate the divided or summed
   * height rather than re-estimate. Omitting it is allowed — a host that only wants the
   * new ordering can fall back to estimates — but then it cannot preserve the scroll
   * position across the change.
   */
  reindex(ids: string[], change?: GeometryChange): void
  /** Called once the registry and the geometry agree again, with the caret anchor. */
  onApplied?(result: LifecycleResult, focus: CaretAnchor): void
}


/**
 * A caret position that survives a structural change.
 *
 * `offset` is characters from the start of the section's *text content*, not a
 * ProseMirror document position. A document position counts tokens and shifts when
 * the block structure changes — which is exactly what a split does — whereas a
 * character offset is a property of the text and is stable across a division.
 */
export interface CaretAnchor {
  sectionId: string
  offset: number
}

/**
 * A caret position expressed as an offset into its document's text.
 *
 * # The measure, and why it is `textBetween`
 *
 * `doc.textBetween(0, pos, '\n').length` — every character before `pos`, plus one per
 * completed block. That makes it a property of the *text*, so it survives a split:
 * the head keeps blocks `[0, cut)` and the tail keeps `[cut, end)`, and a position in
 * either half has exactly the same text before it as before the split.
 *
 * A ProseMirror document position does not have that property — it counts structural
 * tokens, so it changes when the block structure does, which is exactly what a split
 * does. That is the whole reason an offset exists.
 *
 * # The alternatives, and why they are worse
 *
 * `ResolvedPos.textOffset` is the obvious candidate and is wrong here: it is
 * *depth-relative* and resets at every text block, so two positions a thousand
 * characters apart in different paragraphs can both have `textOffset === 0`. Measured
 * on a three-block document: `textOffset` goes 0,1,0,0,1,0,0,0,1,2,3,0,0,0,1,0.
 *
 * Counting text nodes directly — summing `node.text.length` and one per text block —
 * is also wrong, and was the first implementation here. A hand-rolled `nodesBetween`
 * walk has to reproduce ProseMirror's descend semantics, its partial-node handling at
 * the range boundary, and its treatment of inline nodes. Getting any of those subtly
 * wrong produces offsets that are wrong only at block boundaries, which is to say
 * wrong exactly where a caret is most noticeable.
 *
 * # What it is not injective over
 *
 * Two positions can share an offset: inside a paragraph, a `hardBreak` contributes
 * nothing to `textBetween`, so "after the last character" and "at the break" are the
 * same measure. No text-length measure can distinguish them. The round-trip property is
 * therefore idempotence, not identity — see [`positionFromCaretOffset`].
 */
export function caretOffsetFromStart(doc: TextMeasurable, pos: number): number {
  return doc.textBetween(0, pos, '\n').length
}

/** The slice of ProseMirror's `Node` these functions need. */
export interface TextMeasurable {
  textBetween(from: number, to: number, blockSeparator?: string | null): string
  content: { size: number }
}

/**
 * The first position whose offset is at least `offset`.
 *
 * # Binary search, because a linear scan is quadratic
 *
 * `textBetween` is O(the range), so testing every position would be O(n²) — about 80
 * million character visits on a 9000-character section. It runs on a structural
 * change, which is rare, but "rare" is how O(n²) code survives until a 2000-page
 * document makes it common.
 *
 * # Why it is a search for the *first* match
 *
 * The measure is non-decreasing in `pos`, so several positions can reach a given
 * offset. The first is the canonical one: the earliest place with that much text
 * before it, which is the start of the character the user was pointing after. Later
 * matches are inside the same character or at a block boundary, and choosing among
 * them arbitrarily would make the restored caret depend on the scan order.
 *
 * The monotonicity the search relies on is asserted in `test/lifecycle.ts` rather than
 * assumed, because a non-monotone measure would make the search return *a* position
 * rather than the first one, with no error.
 *
 * # Clamping
 *
 * An offset past the end resolves to the last valid position. A merge shortens the
 * section the anchor was in, and a caret at the join point is a good outcome; throwing
 * would leave the user with no caret at all, which is worse and much harder to notice
 * being wrong.
 */
export function positionFromCaretOffset(doc: TextMeasurable, offset: number): number {
  const last = doc.content.size - 1
  if (offset <= 0) return 1
  // A full-document measure at or above the offset means nothing was lost; the last
  // position is where the text ends.
  if (caretOffsetFromStart(doc, last) < offset) return last

  let lo = 1
  let hi = last
  while (lo < hi) {
    const mid = (lo + hi) >> 1
    if (caretOffsetFromStart(doc, mid) >= offset) hi = mid
    else lo = mid + 1
  }
  return lo
}

/** Build the split action for a section, or null if there is nothing to split. */
export function splitActionFor(
  sectionId: string,
  index: number,
  json: unknown,
): LifecycleAction | null {
  const cut = chooseCutIndex(json)
  if (cut === null) return null
  return { kind: 'split', section_id: sectionId, at_block: cut, index }
}

/**
 * Apply a split and reconcile the frontend against the result.
 *
 * # Why the ordering is rebuilt rather than patched
 *
 * See the module header. The short version: `section_ids` is the authoritative new
 * ordering, and applying it wholesale means there is no state in which the registry
 * and the geometry disagree about how many sections exist.
 *
 * # Why the records for the new section come from the reply, not from a re-read
 *
 * The reply carries ids and order, not content. The new section's content is a slice
 * of the section that was split, which the frontend already holds in the editor it
 * was just typing in. So the content is taken from there — which is also more
 * accurate than asking Rust for it, because Rust's copy is what was last *saved*,
 * and an unsaved keystroke would be missing from it.
 */
export async function applySplit(
  host: LifecycleHost,
  sectionId: string,
  json: unknown,
  anchor: CaretAnchor,
): Promise<LifecycleResult> {
  const ids = host.sectionIds()
  const index = ids.indexOf(sectionId)
  if (index < 0) {
    throw new Error(`cannot split ${sectionId}: it is not in the document`)
  }
  const action = splitActionFor(sectionId, index, json)
  if (!action) {
    // Not an error. A one-block section cannot be divided, and `chooseCutIndex`
    // already decided that; the coordinator's job is to not ask.
    return { applied: false, section_ids: [], reason: 'no interior block boundary to cut at' }
  }

  const result = await host.commit(action)
  if (!result.applied) return result

  // A split adds exactly one section. Checking that, rather than only that
  // `newIds[index + 1]` happens to exist, is what makes this a contradiction check:
  // the weaker form passed a reply that removed a section instead of adding one,
  // because the id at that position was still there. The consequence would be the new
  // section's content attached to an existing section, and a document silently
  // reordered for the rest of the session.
  const newIds = result.section_ids
  if (newIds.length !== ids.length + 1) {
    throw new Error(
      `split reported success but its ordering has ${newIds.length} ids for ` +
        `${ids.length + 1} sections`,
    )
  }

  // The new section sits immediately after the one we split, because that is where the
  // order key was allocated — so the id at `index + 1` is the new one, whatever it is
  // called.
  const newId = newIds[index + 1]!
  if (newId === sectionId) {
    throw new Error(
      `split reported success but its ordering still has ${sectionId} at index ${index + 1}`,
    )
  }

  const nodes = topLevelNodes(json)
  const cut = action.kind === 'split' ? action.at_block : 0
  const head = { type: 'doc', content: nodes.slice(0, cut) }
  const tail = { type: 'doc', content: nodes.slice(cut) }

  host.registry.record(sectionId)!.json = head
  host.registry.record(sectionId)!.loaded = true
  host.registry.record(sectionId)!.metrics = localMetrics(head)

  // # The mounted editors must be told, and not telling them is a data-loss bug
  //
  // The records above now describe two sections, but a *mounted* editor still holds the
  // pre-split document. Measured on a 20-block section split in half:
  //
  // ```
  //   record (persisted) : 10 blocks, 910 words
  //   editor (on screen) : 20 blocks, 1820 words
  // ```
  //
  // Three things follow, and all three are bad:
  //
  // 1. The next keystroke commits the editor's content, and `save_section` writes 20 blocks
  //    to a row that is supposed to hold 10 — **the tail's text is resurrected into the
  //    head**, duplicating it and undoing the split entirely.
  // 2. The next `onChange` recomputes 1820 words, sees it over the 1,500-word limit, and
  //    splits *again*, inventing a third section from a document that was just divided.
  // 3. The user's caret sits in an editor whose content no longer matches anything stored.
  //
  // This is why `test/oscillation.ts` could not get a prune to run: the spurious second
  // split moved the tail one position away from its predecessor, and the prune was
  // *correctly* refused for not being at a seam.
  //
  // `setContent` rather than a document transaction, because the replacement is not a user
  // edit and must not enter the undo stack as one — the split is already an undo entry, and
  // undoing it should restore the original text, not replay two edits.
  if (host.registry.editorIfMounted?.(sectionId)) {
    host.registry.editorIfMounted(sectionId)!.commands.setContent(head)
  }

  const tailRecord: SectionRecord = {
    id: newId,
    json: tail,
    // Real content, sliced out of the section that was just split -- and more current than
    // the row Rust has, which is what was last *saved*.
    loaded: true,
    metrics: localMetrics(tail),
    dirty: true,
  }
  // Inserted after the section it came from, so the registry's order matches the
  // reply's. `insert` shifts the ones after it, which is what the order keys did too.
  host.registry.insert(index + 1, tailRecord)

  // Same for the tail, if it happens to be mounted. It usually is not — the scroller mounts
  // a window and the new section is past its edge — but "usually" is not a guarantee, and a
  // mounted editor holding the whole pre-split document is the bug above again.
  if (host.registry.editorIfMounted?.(newId)) {
    host.registry.editorIfMounted(newId)!.commands.setContent(tail)
  }

  // The one measured height for this section is now a height for two. Saying so is
  // what lets the host divide it in proportion instead of dropping both sections back
  // to estimates, which is what would make the scrollbar lurch on every split.
  //
  // `chars` is the same measure `blockWeight` balances seams by, and unlike blocks it
  // is monotonic in content, so the proportion it produces is the proportion of the
  // rendered height. A zero on both sides means there is no text to weigh -- two
  // empty sections -- and the host falls back to splitting evenly.
  host.reindex(newIds, {
    kind: 'split',
    sectionId,
    headChars: host.registry.record(sectionId)!.metrics.chars,
    tailChars: tailRecord.metrics.chars,
  })

  host.onApplied?.(result, anchor)
  return result
}

/**
 * Apply a merge and reconcile.
 *
 * The frontend has already performed the content merge — `registry.mergeBackward`
 * moves the first block into the previous section, because that has to happen in the
 * live editors for undo to see it as one action. So this reports it to Rust rather
 * than asking Rust to do it, and the section that disappears is the one the caret
 * left.
 */
export async function applyMerge(
  host: LifecycleHost,
  sectionId: string,
  intoSectionId: string,
  anchor: CaretAnchor,
): Promise<LifecycleResult> {
  const ids = host.sectionIds()
  const index = ids.indexOf(sectionId)
  if (index < 0) {
    throw new Error(`cannot merge ${sectionId}: it is not in the document`)
  }
  const result = await host.commit({
    kind: 'merge',
    section_id: sectionId,
    into_section_id: intoSectionId,
    index,
  })
  if (!result.applied) return result

  host.registry.remove(sectionId)
  // The surviving section now holds both sections' content, so its height is the sum
  // of the two. Adding rather than re-estimating is what keeps the document's total
  // height — and therefore the scroll position — unchanged by a merge.
  host.reindex(result.section_ids, { kind: 'merge', intoSectionId, removedId: sectionId })
  // The anchor is resolved here rather than left to `onApplied`, because this function
  // is what knows the content moved into `into_section_id`. A host that had to
  // reconstruct that fact would be reconstructing something the caller already had.
  host.onApplied?.(result, { sectionId: intoSectionId, offset: anchor.offset })
  return result
}

/**
 * A section's counts, computed from its content in the renderer.
 *
 * # This duplicates `holonomy_core::store::analyze`, deliberately and narrowly
 *
 * `analyze` is the authority: it runs on every write and its numbers are what the
 * height model is calibrated against. It cannot be used at the two moments that need a
 * count here, though.
 *
 * **The split trigger** runs on every keystroke, before anything is saved. Waiting for
 * a round trip to learn whether a section has passed 1500 words would put the store on
 * the typing path, which is the one thing the WAL exists to avoid.
 *
 * **A split** produces two new sections that need counts immediately, because
 * `estimateHeight` refuses a section with no block count — by design, after the
 * character-derived fallback proved to be 225% out. The new tail section has no stored
 * row yet, so there is nothing to read.
 *
 * So the numbers are recomputed locally and Rust recomputes the same ones on the next
 * save. Both agree on the rules, which is what `test/lifecycle.ts` checks against
 * `analyze`'s definition:
 *
 * - **words** — whitespace-separated, with a separator at every block boundary so two
 *   paragraphs of `"alpha"` and `"beta"` are two words.
 * - **chars** — *non*-whitespace characters, matching `analyze`. Counting raw length
 *   would inflate every estimate, because a 1500-word paragraph has proportionally more
 *   spaces than a list.
 * - **blocks** — top-level children. This is *not* `analyze`'s whitelist, and the
 *   difference is defensible: `analyze` walks arbitrary JSON and cannot otherwise tell
 *   a block from an inline node, whereas this content came out of a ProseMirror
 *   document whose top-level content spec is `block+`. Every top-level child of a real
 *   document is a block.
 *
 * # Why the separator matters, and which direction the error must go
 *
 * Words must not run across a block boundary: without a separator, `"one two three"`
 * followed by `"four"` reads as two words instead of four. `analyze` inserts a newline
 * at every line-breaking node for exactly this reason.
 *
 * The first version here concatenated with no separator and under-counted a test
 * document by more than half — 3 words instead of 7. Under-counting is the dangerous
 * direction: it means a section over the limit does not split, and the symptom is a
 * section that grows without bound while the code reports it is fine. Over-counting only
 * splits a little early, which is visible and harmless.
 *
 * # Marks are exact, not approximated
 *
 * An earlier version returned `marks: 0` on the grounds that the ceiling was a
 * backstop. It is not: 3000 marks is reachable at well under 1500 words in a heavily
 * formatted section, and a permanent zero would mean such a section never split.
 */
export function localMetrics(json: unknown): {
  words: number
  marks: number
  chars: number
  blocks: number
} {
  const { text, marks } = walkText(json)
  return {
    words: text.split(/\s+/).filter(w => w.length > 0).length,
    marks,
    chars: text.replace(/\s/g, '').length,
    blocks: topLevelNodes(json).length,
  }
}

/** Every text character in a document, with a separator at each block boundary. */
function walkText(json: unknown): { text: string; marks: number } {
  let text = ''
  let marks = 0
  const visit = (node: unknown): void => {
    if (!node || typeof node !== 'object') return
    const n = node as { type?: string; text?: string; marks?: unknown[]; content?: unknown[] }
    if (typeof n.text === 'string') {
      text += n.text
      marks += Array.isArray(n.marks) ? n.marks.length : 0
      return
    }
    if (Array.isArray(n.content)) n.content.forEach(visit)
    // Close the block with a separator, so two paragraphs of `"alpha"` and `"beta"` are
    // two words and not one. Matches `analyze`'s newline at line-breaking nodes.
    //
    // Tested against the text *after* the children are visited. An earlier version
    // captured it before, so the first block never closed itself and the rest closed
    // inconsistently: 4 words counted where 6 exist. Under-counting is the dangerous
    // direction, because it means an over-limit section does not split.
    if (text.length > 0 && !text.endsWith('\n') && BLOCK_TYPES.has(n.type ?? '')) text += '\n'
  }
  const doc = json as { content?: unknown[] }
  if (Array.isArray(doc?.content)) doc.content.forEach(visit)
  return { text, marks }
}

/**
 * Top-level node types that occupy a line box.
 *
 * Not the same list as `analyze`'s `BLOCK_TYPES`, and deliberately smaller: this one
 * only decides where to put a *separator in extracted text*, so it can afford to
 * include anything that renders on its own line without claiming to be the geometry's
 * definition. Duplicating the geometry's whitelist here would be the thing to avoid.
 */
const BLOCK_TYPES = new Set([
  'paragraph',
  'heading',
  'bulletList',
  'orderedList',
  'taskList',
  'blockquote',
  'codeBlock',
  'table',
  'horizontalRule',
])
