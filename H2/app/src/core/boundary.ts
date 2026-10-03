/**
 * Keyboard traversal across section boundaries.
 *
 * # The problem
 *
 * With one editor per section (M0 strategy C), a section boundary is a hard edge
 * in the editing experience. ProseMirror knows nothing about the neighbouring
 * section, so the default keybindings do the wrong thing at the seam:
 *
 * - `ArrowUp` at position 0 stays put. The user expects to cross into the
 *   previous section.
 * - `ArrowDown` at the end position stays put. Same expectation, other direction.
 * - `Backspace` at position 0 does nothing. The user expects it to merge into
 *   the previous section, which is how every other editor behaves at a
 *   paragraph break.
 *
 * # The approach
 *
 * A ProseMirror plugin on each editor intercepts these keys *before* the default
 * handling, decides whether the caret is at the relevant edge, and if so hands
 * off to a coordinator that knows about the other sections. If the coordinator
 * cannot service the request (no previous section, or the neighbour is not
 * mounted) the key is allowed to fall through to the default, so behaviour
 * degrades to "nothing happens" rather than "something breaks".
 *
 * `ArrowUp`/`ArrowDown` are the interesting case. Crossing should place the
 * caret at the *end* (or start) of the neighbour, but a naive jump loses the
 * user's horizontal position, so the target column is preserved. That is done
 * with ProseMirror's own coordinate translation rather than a guess: measure the
 * caret's on-screen x before crossing, then ask the target view which document
 * position sits at that x on the line nearest the seam. See `CrossingHint`.
 *
 * `Backspace` merging is deliberately conservative: it only merges when the
 * caret is at the very start of the first block, and it merges the first block
 * into the last block of the previous section. Merging arbitrary text is left
 * to the storage layer's section splitter, which owns the word and mark
 * thresholds and can rebalance properly.
 */

import { Extension, type Editor } from '@tiptap/core'
// `Fragment`, `Node` and `Slice` live in prosemirror-model, not
// prosemirror-state. Importing them from @tiptap/pm/state fails at module load
// with "does not provide an export named 'Fragment'".
import { Fragment, Node as PMNode } from '@tiptap/pm/model'
import { Plugin, PluginKey } from '@tiptap/pm/state'

/**
 * How to choose the caret position after crossing a seam.
 *
 * # Why a coordinate rather than a character offset
 *
 * The first version carried a character offset from the section edge and hoped
 * the neighbour's line structure was similar. That does not survive contact with
 * real formatting: an offset of 8 lands mid-word in a 40-character line and at
 * the end of a 5-character one, so crossing between two differently-formatted
 * paragraphs would jump the caret somewhere unrelated.
 *
 * What the user actually perceives is a *screen* column, and ProseMirror can
 * convert in both directions:
 *
 *   - `view.coordsAtPos(pos)` gives the on-screen rectangle of a document
 *     position, so the horizontal column is read from the live layout rather
 *     than inferred.
 *   - `view.posAtCoords({left, top})` maps a screen point back to a document
 *     position in *that view*, so the target position is derived from the
 *     target's own layout.
 *
 * The vertical coordinate is the seam, since we are moving to the line adjacent
 * to the boundary: the bottom edge of the target for an upward crossing, the top
 * edge for a downward one. The 4px inset puts the point inside the line box
 * rather than on the boundary between two lines, where `posAtCoords` is
 * ambiguous.
 */
export interface CrossingHint {
  /** The caret's on-screen left edge, from `coordsAtPos`, in client coordinates. */
  left: number
  /**
   * Which side of the target to probe for the line. `'end'` walks up from the
   * target's bottom edge, `'start'` walks down from its top.
   */
  side: 'start' | 'end'
}

export type CrossingTarget =
  /** Preferred: resolve the position from measured screen coordinates. */
  | { kind: 'coords'; hint: CrossingHint }
  /** Fallback when the target is not laid out (offscreen, zero-size). */
  | { kind: 'offset'; chars: number }

/** What the coordinator must provide to service a boundary crossing. */
export interface BoundaryHost {
  /** The section before the given one, or null at the start of the document. */
  previousSection(sectionId: string): string | null
  /** The section after the given one, or null at the end. */
  nextSection(sectionId: string): string | null
  /**
   * Move focus to a section, mounting it if needed, and place the caret.
   *
   * `target` may be a measured coordinate hint, or an offset fallback. The host
   * must try the hint first: it is the only representation that survives
   * differing line lengths on either side of the seam.
   */
  focusEdge(sectionId: string, edge: 'start' | 'end', target: CrossingTarget): boolean
  /**
   * Merge the first block of `sectionId` into the tail of `previousSectionId`.
   * Returns true if the merge happened.
   */
  mergeBackward(sectionId: string, previousSectionId: string): boolean
/**
 * Remove `sectionId` from the document entirely, because it is empty.
   *
   * # Why this exists separately from `mergeBackward`
   *
   * Backspace at position 0 of section K has two genuinely different jobs, and which one
   * is right depends on whether K has any content:
   *
   * - **K is non-empty.** Merging K's first block into K−1 is what undoes a split, and
   *   that is the behaviour `mergeBackward` implements and the seam-reversibility rule in
   *   `lifecycle.ts` is built around.
   * - **K is empty.** There is nothing to merge. The user deleted the last character and
   *   is now pressing Backspace again; what they mean is "get rid of this section", not
   *   "concatenate two empty things". Merging here would leave an empty section wedged
   *   between two populated ones forever, each Backspace shifting the seam without ever
   *   removing it.
   *
   * The empty case is not a special case of the merge, it is the absence of one: a merge
   * moves content, and there is no content. So it is a prune, and it has to reach the
   * manifest, SQLite, and the geometry — none of which `mergeBackward` touches.
   *
   * Returns true if the section was removed.
   */
  pruneSection?(sectionId: string, previousSectionId: string): boolean
}

const boundaryKey = new PluginKey('holonomy-boundary')

/**
 * How far inside the target's edge to probe for the line to land on.
 *
 * A point exactly on the boundary between two line boxes is ambiguous to
 * `posAtCoords`, so the probe is inset.
 */
const SEAM_INSET_PX = 4

/**
 * Measure the caret's screen column, for use as a crossing hint.
 *
 * Returns null if the position has no layout box, which happens when the editor
 * is not rendered (a headless mount, or a section the window has not yet laid
 * out). The caller falls back to a character offset in that case.
 */
function measureCaretColumn(view: any, pos: number): number | null {
  try {
    const coords = view.coordsAtPos(pos)
    if (!coords || typeof coords.left !== 'number' || !Number.isFinite(coords.left)) return null
    // A zero-width editor is not laid out; a coordinate of exactly 0 is
    // indistinguishable from a real caret at the left margin, and resolving it
    // would land the caret at the paragraph start rather than the intended
    // column. Treat a collapsed rect as unmeasured.
    if (coords.right === coords.left && coords.bottom === coords.top) return null
    return coords.left
  } catch {
    return null
  }
}

/**
 * Install the boundary plugin on an editor.
 *
 * # This must return a Tiptap `Extension`, not a ProseMirror `Plugin`
 *
 * An earlier version returned a bare `Plugin` for the caller to drop into the
 * `extensions` array. Tiptap accepts only `Extension` instances there and
 * **silently discards** anything else, so the plugin never ran: no arrow key
 * crossed a boundary and no Backspace merged. The symptom was a registry that
 * reported a boundary plugin present in source, and an editor whose
 * `state.plugins` did not contain it.
 *
 * This is the same class of bug as M0 finding #3. The test
 * `diagnose: are the boundary plugin and undo coordinator wired?` checks
 * `state.plugins` directly rather than trusting that the extension was passed in,
 * which is what caught it.
 *
 * It closes over `sectionId` and `host`, so it is constructed per editor.
 */
export function boundaryTraversal(sectionId: string, host: BoundaryHost): Extension {
  return Extension.create({
    name: 'holonomyBoundary',

    addProseMirrorPlugins() {
      return [
        new Plugin({
          key: boundaryKey,

          props: {
            handleKeyDown(view, event) {
              const { state } = view
              const { empty, from, to } = state.selection

              // Selection spanning blocks: let ProseMirror handle it. Crossing a
              // boundary mid-selection would be surprising and is not required.
              if (!empty) return false

              // --- ArrowUp at the very start --------------------------------
              if (event.key === 'ArrowUp') {
                if (!atStartOfFirstBlock(state)) return false
                const prev = host.previousSection(sectionId)
                if (!prev) return false // start of document: default no-op
                event.preventDefault()
                host.focusEdge(prev, 'end', crossingTarget(view, from, 'end'))
                return true
              }

              // --- ArrowDown at the very end --------------------------------
              if (event.key === 'ArrowDown') {
                if (!atEndOfLastBlock(state)) return false
                const next = host.nextSection(sectionId)
                if (!next) return false
                event.preventDefault()
                host.focusEdge(next, 'start', crossingTarget(view, from, 'start'))
                return true
              }

              // --- Backspace at the very start ------------------------------
              if (event.key === 'Backspace' && from === to) {
                if (!atStartOfFirstBlock(state)) return false
                const prev = host.previousSection(sectionId)
                if (!prev) return false
                event.preventDefault()

                // An empty section is pruned rather than merged. See `pruneSection` on
                // the host interface for why these are two different operations and not
                // one operation with a special case.
                //
                // Checked here, against the live document, because "empty" is a property
                // of what the user typed rather than of anything the registry recorded.
                if (isSectionEmpty(view.state) && host.pruneSection) {
                  if (host.pruneSection(sectionId, prev)) return true
                  // The prune was declined — most likely because the section is the last
                  // one, or the host is mid-operation. Fall through to the merge rather
                  // than swallowing the keystroke, so the key never becomes inert.
                }

                host.mergeBackward(sectionId, prev)
                return true
              }

              return false
            },
          },
        }),
      ]
    },
  })
}

/**
 * Build the target for a crossing, measuring the caret's column if possible.
 *
 * The measurement is taken *before* the host changes focus, because afterwards
 * the source editor loses DOM focus and `coordsAtPos` returns a stale or
 * collapsed rect.
 */
function crossingTarget(view: any, pos: number, side: 'start' | 'end'): CrossingTarget {
  const left = measureCaretColumn(view, pos)
  if (left === null) return { kind: 'offset', chars: 0 }
  return { kind: 'coords', hint: { left, side } }
}

/**
 * Resolve a crossing target to a document position in `editor`.
 *
 * The coordinate path is tried first and is the only one that respects the
 * target's own line lengths. The offset path is a fallback for when the target
 * is not laid out, in which case a guess at the edge is better than refusing to
 * move the caret at all.
 */
function resolveTarget(editor: Editor, edge: 'start' | 'end', target: CrossingTarget): number | null {
  if (target.kind === 'coords') {
    const pos = posFromCoords(editor, target.hint)
    if (pos !== null) return pos
    return edgePosition(editor, edge, 0)
  }
  return edgePosition(editor, edge, target.chars)
}

/**
 * Map a screen column back to a document position, using the target view's own
 * layout.
 *
 * The probe y is derived from the target's box and the seam side: for an upward
 * crossing the interesting line is the last one, for a downward crossing the
 * first. Returns null when the point falls outside any text, e.g. in the margin,
 * so the caller can fall back rather than landing somewhere arbitrary.
 */
function posFromCoords(editor: Editor, hint: CrossingHint): number | null {
  const view = editor.view
  const dom = view.dom as HTMLElement
  let rect: DOMRect
  try {
    rect = dom.getBoundingClientRect()
  } catch {
    return null
  }
  // A zero-height box means the section is not laid out (display:none, or a
  // detached mount). Any y would be a guess, so decline.
  if (rect.height === 0) return null

  const top = hint.side === 'end' ? rect.bottom - SEAM_INSET_PX : rect.top + SEAM_INSET_PX
  // Clamp x into the box: a point beyond the right edge resolves to the line's
  // end, which is what a click past the end of a line does, but a point left of
  // the box may land in the previous section's coordinates and must not be
  // allowed to.
  const left = Math.max(rect.left, Math.min(hint.left, rect.right))

  let found: { pos: number; inside: number } | null = null
  try {
    found = view.posAtCoords({ left, top })
  } catch {
    return null
  }
  if (!found || typeof found.pos !== 'number') return null
  // `inside: -1` means the point is not inside any text node, e.g. in the
  // vertical gap between paragraphs. Accepting it would drop the caret in the
  // wrong place, so decline and let the caller fall back.
  if (found.inside < 0) return null
  return found.pos
}

/**
 * Positional fallback: a character offset from the section edge.
 *
 * Degraded but deterministic. An offset of 8 lands mid-line in a long paragraph
 * and at the end of a short one, which is exactly why the coordinate path above
 * exists; this is only reached when the target has no layout.
 */
function edgePosition(editor: Editor, edge: 'start' | 'end', chars: number): number | null {
  const { doc } = editor.state

  if (edge === 'start') {
    // Mutable holder rather than a narrowed local: TypeScript narrows a
    // `let x: T | null` assigned inside a callback to `never` after the null
    // check, because it cannot see the callback ran.
    const found: { pos: number } = { pos: 0 }
    let have = false
    doc.descendants((node: any, pos: number) => {
      if (have) return false
      if (node.isTextblock) {
        found.pos = pos + 1 + Math.min(chars, node.content.size)
        have = true
        return false
      }
      return true
    })
    return have ? found.pos : null
  }

  const last: { pos: number; size: number } = { pos: 0, size: 0 }
  let haveLast = false
  doc.descendants((node: any, pos: number) => {
    if (node.isTextblock) {
      last.pos = pos
      last.size = node.content.size
      haveLast = true
    }
    return true
  })
  if (!haveLast) return null
  const back = Math.min(chars, last.size)
  const size = doc.content.size
  return Math.max(1, Math.min(last.pos + 1 + (last.size - back), size - 1))
}

/**
 * Does this section hold no content at all?
 *
 * Exported because `SectionRegistry.pruneSection` must answer the same question, and two
 * copies of "is this section empty" would be two definitions — the failure mode the rest
 * of this file keeps returning to.
 *
 * # What counts as empty, and why whitespace is not content
 *
 * A section holding a paragraph of spaces is, to the user, empty: they cannot see
 * anything, and cannot select the spaces without selecting a line. Counting them as
 * content means Backspace merges instead of pruning, and the empty section survives —
 * which is the case this exists to fix.
 *
 * # Why the whole document is inspected, not just the first block
 *
 * The cheaper check is whether the first block is empty, since Backspace is at position 0.
 * That is wrong: a section holding `[empty, "text"]` is *not* prunable, and pruning it
 * would delete a paragraph of the user's writing. Emptiness is a property of the section,
 * not of the block the caret happens to be in.
 *
 * Cost is one pass over the section's text, bounded by the 1,500-word limit that keeps
 * sections small, and it only runs on a keystroke already sitting at a seam.
 *
 * # Why an image counts as content
 *
 * A figure has no characters at all, so a text-only test calls it empty — and pruning
 * that section discards a picture the user inserted. The block walk catches any
 * non-textblock leaf (an image, a rule, an equation). Being wrong here is
 * unrecoverable, so the predicate is deliberately biased toward "not empty".
 */
export function isSectionEmpty(state: any): boolean {
  const doc = state.doc
  if ((doc.textContent ?? '').trim().length > 0) return false

  let hasNonTextContent = false
  doc.descendants((node: any) => {
    if (hasNonTextContent) return false
    if (node.isTextblock) return true
    // A wrapper (table, list, blockquote) is only meaningful if it holds something; the
    // recursion finds that. A leaf block is the thing we are looking for.
    const isLeaf = !node.isBlock || node.childCount === 0
    if (isLeaf && node.type?.name !== 'doc') {
      hasNonTextContent = true
      return false
    }
    return true
  })
  return !hasNonTextContent
}

/**
 * Is the selection at the start of the first block?
 *
 * Checks structural position rather than `pos === 0`, because a section's
 * content may start with a heading or other non-paragraph node.
 */
function atStartOfFirstBlock(state: any): boolean {
  const { $from, empty } = state.selection
  if (!empty) return false
  if ($from.parentOffset !== 0) return false
  // Depth 1 is the first child of the doc node.
  return $from.depth === 1 && $from.index(0) === 0
}

/** Is the selection at the end of the last block? */
function atEndOfLastBlock(state: any): boolean {
  const { $from, empty } = state.selection
  if (!empty) return false
  if ($from.parentOffset !== $from.parent.content.size) return false
  const lastIndex = state.doc.childCount - 1
  return $from.depth === 1 && $from.index(0) === lastIndex
}

/**
 * Place the caret near an edge of a section, preserving the visual column when
 * the target is laid out.
 *
 * Exported for the host to call, so a click-to-focus can use the same placement
 * logic as a keyboard crossing and the two do not drift apart.
 */
export function placeCaretAtEdge(
  editor: Editor,
  edge: 'start' | 'end',
  target: CrossingTarget,
): boolean {
  const pos = resolveTarget(editor, edge, target)
  if (pos === null) return false

  // `posAtCoords` can legitimately return a position that is not a valid text
  // selection point — a position between two blocks, for instance. Asking
  // ProseMirror to select it would throw inside `setTextSelection`, so validate
  // first and decline rather than crash the editor.
  if (!isSelectable(editor, pos)) return false

  editor.commands.setTextSelection(pos)
  return true
}

/** Is `pos` a position a text selection can occupy? */
function isSelectable(editor: Editor, pos: number): boolean {
  try {
    const { doc } = editor.state
    if (pos < 0 || pos > doc.content.size) return false
    const $pos = doc.resolve(pos)
    return $pos.parent.isTextblock
  } catch {
    return false
  }
}

/**
 * Convert a block's JSON into a `Fragment` suitable for `tr.insert`.
 *
 * # Why this exists
 *
 * `Transaction.insert(pos, content)` takes a **`Fragment`**. Two wrong shapes
 * were tried first, and both fail identically at every position, so they read as
 * position bugs rather than type bugs:
 *
 *   - the plain JSON object from `node.toJSON()`
 *     -> `Can not convert [object Object] to a Fragment`
 *   - a `Slice` built from that node
 *     -> `Can not convert <heading("Section 1")>(0,0) to a Fragment`
 *
 * `tr.replace` accepts a `Slice`; `tr.insert` does not. Verified against four
 * call shapes; `Fragment.from(node)` at `doc.content.size` is the one that
 * appends a child.
 *
 * The node is re-parsed through the target document's own schema, so the
 * fragment is validated against the content model it has to fit.
 */
export function jsonToFragment(doc: any, json: unknown): Fragment | null {
  try {
    const node = PMNode.fromJSON(doc.type.schema, json as any)
    return Fragment.from(node)
  } catch {
    // A block that does not fit this schema (an unknown node type, say) is
    // reported rather than inserted, so the caller can leave it alone.
    return null
  }
}
