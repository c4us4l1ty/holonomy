/**
 * Images, addressed by content and nothing else.
 *
 * # What this closes
 *
 * The asset table, the `holo-asset://` scheme and the no-base64 invariant all existed with
 * no node that could hold an image, so no document could actually contain a figure. That is
 * not a missing feature so much as a missing *seam*: the storage layer was finished and the
 * document had nowhere to put the bytes.
 *
 * # Why `src` must be an asset URL and nothing else
 *
 * Because the alternative is a `data:` URL in section JSON, and `assets.ts` explains at
 * length why that is the failure this project is arranged to prevent: a third more bytes,
 * re-parsed on every read of the section, and copied by every keystroke that snapshots it.
 *
 * Enforced at the node rather than by convention, because a convention is not a thing. Two
 * places enforce it, deliberately:
 *
 * - `addAttributes`'s `parseHTML` drops anything that is not an asset address, so a
 *   document pasted in from elsewhere does not smuggle a base64 payload in through the door.
 * - The NodeView's `update` refuses a `src` change to a non-asset URL, so a transaction that
 *   sets one is ignored rather than rendered.
 *
 * The second is a refusal to *render*, not to store. Typing a URL into a paragraph and
 * setting it as an image attribute is a user action that should fail visibly, and it does:
 * the image keeps its previous content. What must never happen is a `data:` URL reaching
 * persisted JSON, and the first enforcement is what stops that.
 *
 * # The NodeView owns the reference, entirely
 *
 * Acquire here, release here, in `destroy`. One owner, one lifetime.
 *
 * The first version acquired here *and* released from the registry's unmount path, on the
 * reasoning that the two lifecycles are different — a NodeView exists while a node is in a
 * *rendered* editor, while a section outlives any one editor. The reasoning was right and the
 * arithmetic was not: the section unmount destroys the editor, which destroys the NodeView, so
 * two acquisitions per section met one release, and the count climbed with every scroll. A
 * figure revisited a hundred times would hold a hundred references and revoke none of them,
 * which is precisely the leak the refcount exists to prevent — and it is invisible in a test
 * that only checks "released at the end".
 *
 * So the count is per *rendered node*, which is the thing that actually holds the bytes: a
 * rendered `<img>` keeps its `Blob` alive, and an unrendered one holds nothing. That is the
 * quantity worth bounding, and it is the one the resolver can see.
 */

import { Node, mergeAttributes } from '@tiptap/core'
import type { NodeViewRendererProps } from '@tiptap/core'
import { assetHashFromUrl, assetUrl, isInlineDataUrl } from './assets.js'

/**
 * What the NodeView needs from the session.
 *
 * `release` is here as well as `acquire` because a NodeView that changes a node's `src` has
 * to give up the old reference immediately — the figure it was holding is no longer this
 * node's claim, and waiting for the section's unmount would keep the bytes for the rest of
 * the session. The registry still does the releasing for the unmount itself; this covers only
 * the mid-life replacement.
 */
export interface ImageHost {
  /** Take a reference and return a URL an `<img src>` can load. */
  acquire(hash: string): Promise<string | null>
  /** Give up one reference. */
  release(hash: string): void
}

/**
 * The NodeView.
 *
 * # Why the `src` is assigned asynchronously
 *
 * Because acquiring is asynchronous — it may have to fetch, and it may have to probe the
 * engine's transport — and a NodeView's DOM exists before that resolves. Assigning
 * synchronously would either block the scroll path on an IPC round trip or render nothing
 * and hope.
 *
 * So the element starts with no `src`, and the `load` class is applied when the URL lands.
 * That means a figure has a layout box with the right dimensions from the first frame —
 * from `width`/`height`, which are real and are in the document — rather than collapsing and
 * reflowing the section as each image arrives. A section whose height changes as you scroll is
 * exactly the failure the whole geometry exists to prevent.
 *
 * # Why a failed acquire renders a visible placeholder
 *
 * Because an asset that cannot be fetched — one whose section arrived before its bytes, on a
 * device mid-sync — is a *state*, not an error. A broken-image glyph is a state too, but it
 * says nothing and occupies nothing, and the geometry then has a hole in it. A placeholder
 * with the figure's dimensions keeps the scroll height honest and says what is missing.
 */
function createImageNodeView(host: ImageHost) {
  return (props: NodeViewRendererProps) => {
    const { node } = props
    const dom = document.createElement('img')
    dom.className = 'holo-image'
    dom.alt = String(node.attrs.alt ?? '')
    dom.decoding = 'async'

    // Real dimensions from the document, not measured. `width`/`height` are attributes the
    // caller sets when inserting, so the layout box is right before the bytes arrive.
    const width = node.attrs.width
    const height = node.attrs.height
    if (typeof width === 'number' && typeof height === 'number') {
      dom.width = width
      dom.height = height
    }

    /** Set to false when the node is destroyed, so a late resolve cannot touch dead DOM. */
    let live = true

    const apply = (src: string | null) => {
      if (!live) return
      if (src === null) {
        dom.classList.add('holo-image--missing')
        return
      }
      dom.classList.add('holo-image--loaded')
      dom.src = src
    }

    // The hash this NodeView currently holds. Tracked in a local rather than read back off
    // `props.node` in `destroy`, because a mid-life `src` change replaces the node the props
    // describe and `destroy` must release the *last* one this NodeView acquired, not the first.
    let currentHash: string | null = null

    const hash = assetHashFromUrl(node.attrs.src)
    if (hash === null) {
      // Not an asset address. Refusing to render it is the point: a `data:` URL here would be
      // rendered happily by any other image node and land in the document's JSON.
      dom.classList.add('holo-image--refused')
      apply(null)
    } else {
      // Set before the acquire, not after: `destroy` reads this local, and a node destroyed
      // while its acquire is still in flight must still release the reference it was about to
      // take. Setting it in the `.then` would leak exactly that case -- which is the case a fast
      // scroll produces, because the section leaves the window before the fetch lands.
      currentHash = hash
      void host.acquire(hash).then(apply)
    }

    return {
      dom,
      update(updated: any) {
        if (updated.type.name !== node.type.name) return false
        const nextSrc = String(updated.attrs?.src ?? '')
        const nextAlt = String(updated.attrs?.alt ?? '')
        const nextHash = assetHashFromUrl(nextSrc)
        // Refuses a `data:` URL rather than rendering it. See the module header.
        if (nextHash === null) return true
        if (nextSrc === String(node.attrs.src ?? '')) {
          if (nextAlt !== dom.alt) dom.alt = nextAlt
          return true
        }
        // The figure changed, so the old reference is no longer this node's claim. Released
        // here rather than at unmount because the registry's release is keyed on the section's
        // *final* content, and this node's claim ended the moment its `src` changed.
        const previous = currentHash
        // `node` is read-only on the props, which is ProseMirror's way of saying the NodeView
        // must not rewrite the node it was given, so the "what changed" bookkeeping lives in
        // this local rather than by rewriting the node.
        if (previous && previous !== nextHash) host.release(previous)
        currentHash = nextHash
        void host.acquire(nextHash).then(apply)
        return true
      },
      // No `contentDOM`: an image is an atom with nothing to type into.
      //
      // `destroy` is where the reference goes. Not `destroy` plus a release from the section's
      // unmount path: that was two owners, and the count climbed with every scroll.
      destroy() {
        live = false
        if (currentHash !== null) host.release(currentHash)
      },
    }
  }
}

export type AssetImageHost = ImageHost

export const AssetImage = Node.create<{ host: AssetImageHost }>({
  name: 'image',
  group: 'block',
  atom: true,
  selectable: true,
  draggable: true,

  addOptions() {
    // A no-op host rather than an erroring one: an image node installed without a host renders
    // a visible placeholder, which says something useful, where throwing would take down every
    // section that happened to contain a figure.
    return {
      host: {
        acquire: async () => null,
        release: () => {},
      } satisfies AssetImageHost,
    }
  },

  addAttributes() {
    return {
      src: {
        default: '',
        // Anything that is not an asset address decodes to an empty string, so a document
        // pasted in from elsewhere cannot smuggle a `data:` payload into stored JSON. The
        // `assertNoInlineImages` walk is the second line of defence; this is the first.
        parseHTML: el => {
          const value = el.getAttribute('src')
          return value !== null && assetHashFromUrl(value) !== null ? value : ''
        },
        renderHTML: attrs => (attrs.src ? { src: attrs.src } : {}),
      },
      alt: { default: '', parseHTML: el => el.getAttribute('alt') ?? '', renderHTML: a => ({ alt: a.alt }) },
      // Real pixel dimensions, set at insertion time. They are what gives a figure its layout
      // box before its bytes arrive, which is what keeps the scroll height honest.
      width: { default: null, parseHTML: el => Number(el.getAttribute('width')) || null, renderHTML: a => (a.width ? { width: a.width } : {}) },
      height: { default: null, parseHTML: el => Number(el.getAttribute('height')) || null, renderHTML: a => (a.height ? { height: a.height } : {}) },
    }
  },

  parseHTML() {
    // A bare `img` only, and only one whose `src` survives `parseHTML` above.
    return [{ tag: 'img[src]' }]
  },

  renderHTML({ HTMLAttributes }) {
    // A plain `<img>`, never the loaded element: a `renderHTML` result is parsed back, so
    // anything KaTeX-style produced here would be re-parsed as ordinary nodes. See
    // `core/math.ts` for the same reasoning applied to equations.
    return ['img', mergeAttributes(HTMLAttributes, { 'data-asset': 'image' })]
  },

  addNodeView() {
    return createImageNodeView(this.options.host)
  },

  /**
   * Insert a figure from bytes the caller already holds.
   *
   * A command rather than leaving callers to build the node, because the two things that must
   * not go wrong — the hash becoming the address, and the address being validated — are both
   * here, and a caller assembling `{type:'image', attrs:{src}}` by hand has to get both right
   * without being told they exist.
   */
  addCommands() {
    return {
      setAssetImage:
        (attrs: { hash: string; alt?: string; width?: number; height?: number }) =>
        ({ commands }: any) => {
          const src = assetUrl(attrs.hash)
          if (isInlineDataUrl(src)) return false
          return commands.insertContent({ type: this.name, attrs: { ...attrs, src } })
        },
    }
  },
})

declare module '@tiptap/core' {
  interface Commands<ReturnType> {
    assetImage: {
      /** Insert a figure addressed by the SHA-256 of its bytes. */
      setAssetImage: (attrs: {
        hash: string
        alt?: string
        width?: number
        height?: number
      }) => ReturnType
    }
  }
}