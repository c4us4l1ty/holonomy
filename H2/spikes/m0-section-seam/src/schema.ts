import { Node, Mark, mergeAttributes } from '@tiptap/core'

/**
 * Schema for the M0 spike.
 *
 * Two custom inline atoms matter for the seam test because they carry
 * out-of-band payloads: `image` (an asset id) and `inlineEquation` (a
 * pre-rendered SVG). Those are the cases that break naive virtualization.
 */

export const Image = Node.create({
  name: 'image',
  group: 'inline',
  inline: true,
  atom: true,
  draggable: true,
  addAttributes() {
    return {
      src: { default: null },
      alt: { default: null },
      width: { default: null },
      height: { default: null },
    }
  },
  parseHTML() {
    return [{ tag: 'img[src]' }]
  },
  renderHTML({ HTMLAttributes }) {
    return ['img', mergeAttributes(HTMLAttributes)]
  },
})

export const InlineEquation = Node.create({
  name: 'inlineEquation',
  group: 'inline',
  inline: true,
  atom: true,
  addAttributes() {
    return {
      tex: { default: '' },
      svg: { default: '' },
    }
  },
  parseHTML() {
    return [{ tag: 'span[data-equation]' }]
  },
  renderHTML({ HTMLAttributes }) {
    return [
      'span',
      mergeAttributes(HTMLAttributes, { 'data-equation': 'true', contenteditable: 'false' }),
      HTMLAttributes.tex,
    ]
  },
})

export const TextStyle = Mark.create({
  name: 'textStyle',
  addAttributes() {
    return { color: { default: null } }
  },
  parseHTML() {
    return [{ tag: 'span[style]' }]
  },
  renderHTML({ HTMLAttributes }) {
    return ['span', mergeAttributes(HTMLAttributes, { style: `color: ${HTMLAttributes.color}` }), 0]
  },
})

export const Highlight = Mark.create({
  name: 'highlight',
  addAttributes() {
    return { color: { default: '#ffff00' } }
  },
  parseHTML() {
    return [{ tag: 'mark' }]
  },
  renderHTML({ HTMLAttributes }) {
    return ['mark', mergeAttributes(HTMLAttributes), 0]
  },
})

/* ------------------------------------------------------------------ *
 * Section-aware block nodes
 * ------------------------------------------------------------------ */

/**
 * Section membership MUST be a declared schema attribute.
 *
 * The first spike attempt tagged block JSON with `sectionIndex` without
 * declaring it in the schema, and ProseMirror dropped it on parse
 * (`parsedAttrs: [{}, {}]` — see diag.ts). That made every section boundary
 * invisible and collapsed the windowed document to a single empty paragraph
 * (`docSize: 2`). Attributes absent from the schema do not survive
 * `Node.fromJSON`; there is no warning.
 *
 * `rendered: false` keeps the attribute out of the DOM, since it is internal
 * bookkeeping rather than something a user should see.
 */
function sectionAttrs() {
  return {
    sectionIndex: {
      default: null,
      rendered: false,
      parseHTML: (el: HTMLElement) => {
        const v = (el as any).dataset?.sectionIndex
        return v == null ? null : Number(v)
      },
    },
    sectionFocused: {
      default: false,
      rendered: false,
      parseHTML: (el: HTMLElement) => (el as any).dataset?.sectionFocused === 'true',
    },
  }
}

export const SectionParagraph = Node.create({
  name: 'paragraph',
  group: 'block',
  content: 'inline*',
  addAttributes() {
    return sectionAttrs()
  },
  parseHTML() {
    return [{ tag: 'p' }]
  },
  renderHTML({ HTMLAttributes, node }) {
    // Preserve section identity across a DOM round-trip.
    const idx = node.attrs.sectionIndex
    const focused = node.attrs.sectionFocused
    const data =
      idx == null
        ? {}
        : { 'data-section-index': String(idx), 'data-section-focused': String(!!focused) }
    return ['p', mergeAttributes(HTMLAttributes, data), 0]
  },
})

export const SectionHeading = Node.create({
  name: 'heading',
  group: 'block',
  content: 'inline*',
  defining: true,
  addAttributes() {
    return {
      level: {
        default: 1,
        parseHTML: (el: HTMLElement) => Number((el as any).tagName?.slice(1) ?? 1),
      },
      ...sectionAttrs(),
    }
  },
  parseHTML() {
    return [1, 2, 3, 4, 5, 6].map(level => ({ tag: `h${level}` }))
  },
  renderHTML({ HTMLAttributes, node }) {
    const idx = node.attrs.sectionIndex
    const focused = node.attrs.sectionFocused
    const data =
      idx == null
        ? {}
        : { 'data-section-index': String(idx), 'data-section-focused': String(!!focused) }
    return [`h${node.attrs.level}`, mergeAttributes(HTMLAttributes, data), 0]
  },
})

/** Section-aware variants of the remaining block types in the corpus. */
function sectioned<T extends Node<any, any, any>>(base: T, name: string, tag: string) {
  return Node.create({
    name,
    group: (base.options as any).group,
    content: (base.options as any).content,
    defining: true,
    addAttributes() {
      return { ...((base.config as any).addAttributes?.call(base) ?? {}), ...sectionAttrs() }
    },
    parseHTML() {
      return [{ tag }]
    },
    renderHTML({ HTMLAttributes, node }) {
      const idx = node.attrs.sectionIndex
      const data = idx == null ? {} : { 'data-section-index': String(idx) }
      return [tag, mergeAttributes(HTMLAttributes, data), 0]
    },
  })
}
