/**
 * Equations: KaTeX inline and block, as ProseMirror nodes with real NodeViews.
 *
 * # Why a NodeView and not KaTeX's own HTML in a node's `toDOM`
 *
 * Because the rendered HTML has to be *replaced* after the editor builds the DOM, not
 * substituted into it. KaTeX turns a string of TeX into a tree of nested spans and rules
 * with a `<math>`-shaped layout; it has to run after the spans exist in the document, and
 * it has to be re-run when the TeX changes without re-creating the node.
 *
 * `renderHTML` cannot do that — it returns a serialised string and ProseMirror re-parses
 * it, so anything KaTeX produced would be flattened into ordinary nodes on the next parse
 * and the node would stop existing. A `NodeView` owns its own DOM subtree and ProseMirror
 * leaves it alone, which is what lets an equation be one atomic node with one attribute.
 *
 * # Why `atom: true`
 *
 * Because an equation is one thing, not a sequence of editable characters. Marking it an
 * atom is what lets a caret step over it in one press, what makes `selectNode` find it
 * whole, and what makes the seam rule treat it as atomic — which matters, because
 * `ATOMIC_BLOCK_TYPES` lists `mathBlock` and `equation` for exactly that reason.
 *
 * The tension with the seam rule is worth stating: `chooseCutIndex` will not leave an
 * equation as the first block of a new section, because Backspace-at-position-0 refuses to
 * move a non-textblock across a seam. So a long stretch of prose ending in an equation cannot
 * be split there, and the section grows instead. That is the deliberate trade: a section
 * slightly over its limit, rather than a boundary the user cannot cross back.
 *
 * # Why the TeX is stored and the HTML is not
 *
 * `latex` is the attribute and the only source of truth. The rendered HTML is a function of
 * it, so storing both would mean two things to keep in step and a document that could
 * disagree with itself. A document re-opened on a machine with a different KaTeX version
 * re-renders consistently, because the render is derived rather than remembered.
 *
 * # What happens when the TeX is invalid
 *
 * KaTeX throws on malformed input with `throwOnError: false` it renders the source in red
 * instead. That is the choice made here, and it is deliberate: an equation the user is
 * halfway through typing is invalid most of the time, and an editor that blanks the node
 * while they type loses the caret's surroundings and the text around it. Rendering the
 * broken source visibly keeps the node, the position and the surrounding prose intact, and
 * the red styling says what is wrong.
 */

import { Node, mergeAttributes } from '@tiptap/core'
import katex from 'katex'
import type { NodeViewRendererProps } from '@tiptap/core'

/**
 * The two commands these nodes add, declared rather than inferred.
 *
 * # Why the declaration is module-level and not a cast
 *
 * Because `addCommands` checks that what a node returns matches what a caller expects to be
 * able to call, and it can only do that if the command map is declared. A cast would silence
 * the check without giving it anything to check against -- and the thing being checked is the
 * difference between `editor.commands.setInlineMath('x')` type-checking because the command
 * exists and silently doing nothing because it does not.
 */
declare module '@tiptap/core' {
  interface Commands<ReturnType> {
    math: {
      /** Insert an inline equation with the given TeX. */
      setInlineMath: (latex: string) => ReturnType
      /** Insert a display equation with the given TeX. */
      setBlockMath: (latex: string) => ReturnType
    }
  }
}

/** KaTeX's own error class, so a caller can tell "bad TeX" from "bad options". */
export class MathParseError extends Error {}

/**
 * Render TeX to KaTeX's HTML.
 *
 * # Exported so it can be tested without a DOM
 *
 * KaTeX is a pure string-to-string transform, and the interesting question about it here is
 * what it does with input it cannot parse. That question does not need a renderer, and the
 * Node test asserts it in Node; the browser test then covers only the part that does.
 *
 * # `throwOnError: false`
 *
 * See the module header. Throwing would blank the node while the user types, which is the
 * worst thing an editor can do about a temporarily-invalid construct.
 */
export function renderMath(latex: string, displayMode: boolean): string {
  try {
    return katex.renderToString(latex, {
      displayMode,
      throwOnError: false,
      // `strict` off: TeX in a word processor is written by people typing prose, and the
      // canonical errors ("Unicode text character", double superscripts) are about
      // correctness in a LaTeX document rather than in a paragraph of English.
      strict: false,
      // No `\href` and no `\htmlClass`: this HTML is inserted into the editor's DOM, and
      // KaTeX's trust option would be a second thing to get right rather than a guarantee.
      trust: false,
      output: 'html',
    })
  } catch (e: any) {
    // `throwOnError: false` means KaTeX handles per-expression errors itself, so reaching
    // here means something structural -- a missing parser, an option it rejects. Wrapped so
    // a NodeView cannot die and take the editor with it.
    throw new MathParseError(`could not render TeX: ${e?.message ?? String(e)}`)
  }
}

/** The attributes both equation nodes carry. */
export interface MathAttributes {
  /** The TeX source. The only stored form of the equation. */
  latex: string
}

const defaultAttributes: MathAttributes = { latex: '' }

/**
 * Build the NodeView that owns an equation's DOM.
 *
 * # Why `contentDOM` is omitted, and why that is safe
 *
 * A NodeView without a `contentDOM` is a leaf: ProseMirror will not try to put a selection
 * inside it, and the arrow keys step over it. That is what an atom means, and giving it a
 * content hole as well would make the node two things at once — a leaf the caret cannot
 * enter, but with an empty paragraph inside it that the geometry counts and the user can
 * somehow get into.
 */
function createMathNodeView(
  displayMode: boolean,
  tag: 'span' | 'div',
): (props: NodeViewRendererProps) => { dom: HTMLElement } {
  return props => {
    const dom = document.createElement(tag)
    // `data-latex` so the NodeView's own DOM says what it is. ProseMirror does not
    // round-trip a NodeView's DOM through the parser, so the attribute is not how the
    // equation survives a reload — `latex` in the document is. It is here for the
    // inspector, and for a test that asserts what was rendered.
    dom.setAttribute('data-latex', String(props.node.attrs.latex ?? ''))
    dom.className = displayMode ? 'holo-math holo-math--block' : 'holo-math holo-math--inline'

    const draw = (latex: string) => {
      dom.innerHTML = renderMath(latex, displayMode)
      // KaTeX marks its own output so a second render can be found. Kept as an attribute
      // rather than a class because the class is styled.
      dom.setAttribute('data-rendered', 'true')
    }
    draw(String(props.node.attrs.latex ?? ''))

    return {
      dom,
      // Re-render on an attribute change rather than on `update`.
      //
      // `update` returning false makes ProseMirror destroy this NodeView and build another,
      // which would throw away the element and rebuild the subtree — a flicker on every
      // keystroke inside an equation, for an edit the user cannot see the effect of until it
      // finishes. Comparing the one attribute that matters is a cheaper and steadier
      // contract, and it means an update that changes nothing renders nothing.
      update(updated: any) {
        if (updated.type.name !== props.node.type.name) return false
        const next = String(updated.attrs?.latex ?? '')
        if (next === String(props.node.attrs.latex ?? '')) return true
        props.node = updated
        dom.setAttribute('data-latex', next)
        draw(next)
        return true
      },
    }
  }
}

/**
 * Inline equation: `$e^{i\\pi} + 1 = 0$` inside a paragraph.
 *
 * Inline rather than block because that is the overwhelmingly common case in prose, and an
 * inline equation belongs to the flow of its paragraph — including wrapping across lines,
 * which KaTeX's output handles and a block would not.
 */
export const InlineMath = Node.create({
  name: 'inlineMath',
  group: 'inline',
  inline: true,
  atom: true,
  selectable: true,
  draggable: false,

  addAttributes() {
    return { latex: { default: defaultAttributes.latex, parseHTML: el => el.getAttribute('data-latex') ?? '', renderHTML: attrs => ({ 'data-latex': attrs.latex }) } }
  },

  parseHTML() {
    return [{ tag: 'span[data-latex]' }]
  },

  renderHTML({ HTMLAttributes }) {
    // Never the rendered KaTeX markup. A `renderHTML` result is parsed back, so anything
    // produced here would become a tree of spans in the document instead of one equation
    // node -- and `renderHTML` is only used when the NodeView is absent (serialisation to a
  // string, printing, the raw-HTML path), so the markup is a labelled placeholder.
    return ['span', mergeAttributes(HTMLAttributes, { 'data-math': 'inline' }), '']
  },

  addNodeView() {
    return createMathNodeView(false, 'span')
  },

  addCommands() {
    return {
      setInlineMath:
        (latex: string) =>
        ({ commands }: any) =>
          commands.insertContent({ type: this.name, attrs: { latex } }),
    }
  },
})

/**
 * Block equation: a display equation on its own line.
 *
 * A separate node rather than an attribute on the inline one, so the JSON says which it is
 * and a round trip does not have to infer it from context. ProseMirror's parser would have
 * to guess, and a guess that picks wrong turns a display equation into a piece of running
 * text.
 */
export const BlockMath = Node.create({
  name: 'mathBlock',
  group: 'block',
  atom: true,
  selectable: true,
  draggable: false,

  addAttributes() {
    return { latex: { default: defaultAttributes.latex, parseHTML: el => el.getAttribute('data-latex') ?? '', renderHTML: attrs => ({ 'data-latex': attrs.latex }) } }
  },

  parseHTML() {
    return [{ tag: 'div[data-latex]' }]
  },

  renderHTML({ HTMLAttributes }) {
    return ['div', mergeAttributes(HTMLAttributes, { 'data-math': 'block' }), '']
  },

  addNodeView() {
    return createMathNodeView(true, 'div')
  },

  addCommands() {
    return {
      setBlockMath:
        (latex: string) =>
        ({ commands }: any) =>
          commands.insertContent({ type: this.name, attrs: { latex } }),
    }
  },
})

/**
 * Convert pasted TeX between `$…$` and `\[…\]` into equation nodes.
 *
 * # Why paste rather than a markdown input rule
 *
 * Because a word processor's most common source of an equation is another word processor,
 * and `$…$` is what those produce. An input rule only fires on a space typed *inside* the
 * editor, which means pasting a document full of `$x^2$` gets literally that text.
 *
 * # Why it is offered as a function and not installed as a global handler
 *
 * A `handlePaste` that rewrites a paste is a strong claim about the user's clipboard, and
 * it should be enabled deliberately rather than inherited by every document. This is
 * exported so the caller chooses; `main.ts` installs it.
 */
export function texFromText(text: string): Array<{ type: 'inlineMath' | 'mathBlock'; attrs: { latex: string } }> {
  const out: Array<{ type: 'inlineMath' | 'mathBlock'; attrs: { latex: string } }> = []
  // `\[ … \]` first: an inline pattern would match the `\[` and `\]` separately and leave
  // the backslashes behind.
  for (const m of text.matchAll(/\\\[([\s\S]+?)\\\]/g)) {
    out.push({ type: 'mathBlock', attrs: { latex: m[1]!.trim() } })
  }
  const withoutDisplay = text.replace(/\\\[([\s\S]+?)\\\]/g, ' ')
  for (const m of withoutDisplay.matchAll(/(?:^|\s)\$([^$\n]+?)\$(?=\s|$|[.,;:!?])/g)) {
    const latex = m[1]!.trim()
    if (latex) out.push({ type: 'inlineMath', attrs: { latex } })
  }
  return out
}