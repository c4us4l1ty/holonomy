/** Diagnose why strategy B produces an empty document. */
import { chromium } from 'playwright'

const URL = process.env.HOLO_URL ?? 'http://localhost:5183/'
const browser = await chromium.launch()
const page = await browser.newPage()
page.on('pageerror', e => console.error('[pageerror]', e.message, e.stack?.split('\n')[1]))
page.on('console', m => console.log(`[${m.type()}]`, m.text()))

await page.goto(URL, { waitUntil: 'load' })
await page.waitForFunction(() => !!(window as any).HOLO, null, { timeout: 30_000 })

// Build the window doc by hand and ask ProseMirror to parse it, so we can
// see the validation error directly rather than guessing.
const probe = await page.evaluate(() => {
  const out: any = {}
  try {
    const H = (window as any).HOLO
    H.init({ targetWords: 5000, wordsPerSection: 1500, strategy: 'B' })
    const host = document.getElementById('viewport-host')!
    const ed = (window as any).__lastWindowEditor
    out.hostHTML = host.innerHTML.slice(0, 300)
    out.stats = H.stats()
  } catch (e: any) {
    out.error = e.message
    out.stack = e.stack
  }
  return out
})
console.log(JSON.stringify(probe, null, 2))

// Now the decisive test: does setContent with sectionIndex attrs survive?
const attrProbe = await page.evaluate(async () => {
  const { Editor } = await import('/node_modules/@tiptap/core/dist/index.js')
  const StarterKit = (await import('/node_modules/@tiptap/starter-kit/dist/index.js')).default
  const results: any = {}

  const mk = () =>
    new Editor({
      extensions: [StarterKit],
      content: {
        type: 'doc',
        content: [
          { type: 'paragraph', attrs: { sectionIndex: 0, sectionFocused: true }, content: [{ type: 'text', text: 'alpha' }] },
          { type: 'paragraph', attrs: { sectionIndex: 1, sectionFocused: false }, content: [{ type: 'text', text: 'beta' }] },
        ],
      },
    })

  const e1 = mk()
  results.docSize = e1.state.doc.content.size
  results.paragraphCount = e1.state.doc.childCount
  // Read the attrs back off the parsed doc.
  const attrs: any[] = []
  e1.state.doc.forEach((n: any) => attrs.push(n.attrs))
  results.parsedAttrs = attrs
  results.text = e1.state.doc.textContent

  // Now: does a schema that DOES declare the attrs keep them?
  const { Node } = await import('/node_modules/@tiptap/core/dist/index.js')
  const SectionedParagraph = Node.create({
    name: 'paragraph',
    group: 'block',
    content: 'inline*',
    addAttributes() {
      return {
        sectionIndex: { default: null, parseHTML: () => null },
        sectionFocused: { default: false },
      }
    },
    parseHTML() { return [{ tag: 'p' }] },
    renderHTML({ HTMLAttributes }) { return ['p', HTMLAttributes, 0] },
  })
  const e2 = new Editor({
    extensions: [StarterKit.configure({ paragraph: false }), SectionedParagraph],
    content: {
      type: 'doc',
      content: [
        { type: 'paragraph', attrs: { sectionIndex: 0, sectionFocused: true }, content: [{ type: 'text', text: 'alpha' }] },
        { type: 'paragraph', attrs: { sectionIndex: 1, sectionFocused: false }, content: [{ type: 'text', text: 'beta' }] },
      ],
    },
  })
  const attrs2: any[] = []
  e2.state.doc.forEach((n: any) => attrs2.push(n.attrs))
  results.withDeclaredAttrs = attrs2
  results.size2 = e2.state.doc.content.size
  return results
})
console.log('--- attr probe ---')
console.log(JSON.stringify(attrProbe, null, 2))

await browser.close()
