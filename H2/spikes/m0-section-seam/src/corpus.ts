/**
 * Realistic document generator for the M0 spike.
 *
 * The point is to produce a corpus that stresses the things that actually
 * make ProseMirror expensive, rather than uniform lorem ipsum:
 *
 *   - long paragraphs (force line wrapping, many text nodes per mark run)
 *   - heavy inline mark density (bold/italic/underline/color/highlight)
 *   - headings, lists, blockquotes, code blocks
 *   - tables (nested block content in cells)
 *   - images and KaTeX-style equations as atomic inline nodes
 *   - links
 *
 * Word count and section count are both parameterised so we can sweep sizes.
 */

// Deterministic PRNG so runs are comparable across strategies.
export function mulberry32(seed: number): () => number {
  let a = seed >>> 0
  return function () {
    a = (a + 0x6d2b79f5) >>> 0
    let t = a
    t = Math.imul(t ^ (t >>> 15), t | 1)
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61)
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
}

const WORDS = [
  'the', 'a', 'manifold', 'representation', 'of', 'quaternion', 'algebra', 'under',
  'differential', 'cohomology', 'yields', 'a', 'canonical', 'form', 'which',
  'induces', 'natural', 'transformation', 'between', 'cohomology', 'groups',
  'vanishing', 'theorem', 'requires', 'vanishing', 'of', 'higher', 'obstruction',
  'class', 'in', 'degree', 'six', 'we', 'obtain', 'isomorphism', 'between',
  'graded', 'algebras', 'modulo', 'ideal', 'generated', 'by', 'invariants',
  'consider', 'compact', 'Riemannian', 'manifold', 'dimension', 'four', 'with',
  'positive', 'sectional', 'curvature', 'Bonnet–Myers', 'implies', 'diameter',
  'bounded', 'above', 'therefore', 'fundamental', 'group', 'is', 'finite',
  'construction', 'proceeds', 'via', 'desingularization', 'of', 'moduli',
  'stack', 'parametrized', 'by', 'stacky', 'fan', 'where', 'each', 'ray',
  'carries', 'integral', 'weight', 'and', 'cohomology', 'computed', 'via',
  'Čech', 'complex', 'associated', 'to', 'associated', 'cover', 'symmetric',
  'group', 'acts', 'freely', 'away', 'from', 'locus', 'where', 'stabiliser',
  'nontrivial', 'exceptional', 'fibre', 'products', 'appear', 'as', 'trivial',
  'local', 'system', 'in', 'étale', 'chart', 'and', 'therefore', 'splice',
  'well', 'defined', 'functorially', 'category', 'schemes', 'example',
  'motivating', 'analogy', 'comes', 'from', 'period', 'index', 'theory',
  'where', 'matrix', 'entries', 'encode', 'endomorphism', 'algebras', 'up',
  'to', 'conjugacy', 'however', 'convergence', 'rate', 'depends', 'sensitively',
  'upon', 'choice', 'basis', 'and', 'one', 'should', 'choose', 'adaptively',
  'rather', 'than', 'greedily', 'in', 'order', 'avoid', 'pathological',
  'growth', 'observed', 'for', 'naive', 'pivot', 'selection', 'strategies',
]

function randWord(rng: () => number): string {
  return WORDS[Math.floor(rng() * WORDS.length)]
}

function sentence(rng: () => number, targetWords: number): string {
  const parts: string[] = []
  let n = 0
  while (n < targetWords) {
    const len = 3 + Math.floor(rng() * 12)
    const w: string[] = []
    for (let i = 0; i < len; i++) w.push(randWord(rng))
    parts.push(w.join(' '))
    n += len
  }
  const s = parts.join('. ') + '.'
  return s.charAt(0).toUpperCase() + s.slice(1)
}

/** Marks we spray through body text. Weights reflect a formatting-heavy doc. */
const MARK_MIX: Array<{ type: string; attrs?: Record<string, string>; p: number }> = [
  { type: 'bold', p: 0.10 },
  { type: 'italic', p: 0.10 },
  { type: 'underline', p: 0.04 },
  { type: 'strike', p: 0.02 },
  { type: 'code', p: 0.03 },
  { type: 'highlight', attrs: { color: '#ffff00' }, p: 0.02 },
  { type: 'textStyle', attrs: { color: '#b3261e' }, p: 0.02 },
  { type: 'link', attrs: { href: 'https://example.org/ref' }, p: 0.02 },
]

export interface CorpusOptions {
  /** Approximate target word count for the whole document. */
  targetWords: number
  /** Words per top-level section. Sections are the virtualization unit. */
  wordsPerSection: number
  seed?: number
  /** Probability a block is a table rather than a paragraph. */
  tableRate?: number
  /** Probability a block contains an inline image. */
  imageRate?: number
  /** Probability a block contains an inline equation. */
  equationRate?: number
}

export interface GeneratedSection {
  index: number
  wordCount: number
  /** ProseMirror JSON document for this section. */
  json: Record<string, unknown>
}

export interface Corpus {
  sections: GeneratedSection[]
  totalWords: number
}

function textRun(rng: () => number, words: number): string {
  return sentence(rng, words)
}

/**
 * Build one paragraph as an array of ProseMirror text nodes with marks.
 * Mark boundaries are placed at word boundaries, which is what a real
 * formatting pass produces (nobody bolds half a character).
 */
function markedParagraph(rng: () => number, words: number, markDensity: number): Record<string, unknown>[] {
  const content: Record<string, unknown>[] = []
  let remaining = words
  while (remaining > 0) {
    const runWords = Math.max(3, Math.floor(remaining * (0.2 + rng() * 0.5)))
    const text = textRun(rng, Math.min(runWords, remaining))
    remaining -= runWords

    // Decide marks for this run.
    let r = rng()
    let acc = 0
    const marks: Record<string, unknown>[] = []
    for (const m of MARK_MIX) {
      acc += m.p
      if (r < acc && rng() < markDensity) {
        marks.push(m.attrs ? { type: m.type, attrs: m.attrs } : { type: m.type })
      }
    }
    content.push(marks.length ? { type: 'text', marks, text } : { type: 'text', text })
  }
  return content
}

function paragraphBlock(rng: () => number, words: number, opts: Required<Omit<CorpusOptions, 'targetWords' | 'seed'>>): Record<string, unknown> {
  const content = markedParagraph(rng, words, 0.8)

  // Inline atomic nodes: images and equations.
  if (rng() < opts.imageRate) {
    content.push({
      type: 'image',
      attrs: {
        src: `asset-${Math.floor(rng() * 4096).toString(36)}.png`,
        alt: 'figure',
        width: 480,
        height: 320,
      },
    })
  }
  if (rng() < opts.equationRate) {
    content.push({
      type: 'inlineEquation',
      attrs: { tex: String.raw`\int_{\partial M} \omega = \sum_{k} c_k`, svg: `<svg data-eq="${Math.floor(rng() * 1e6)}"/>` },
    })
  }

  return { type: 'paragraph', content }
}

function headingBlock(rng: () => number, level: number): Record<string, unknown> {
  return { type: 'heading', attrs: { level }, content: [{ type: 'text', text: textRun(rng, 4 + Math.floor(rng() * 6)) }] }
}

function listBlock(rng: () => number, items: number): Record<string, unknown> {
  const content: Record<string, unknown>[] = []
  for (let i = 0; i < items; i++) {
    content.push({
      type: 'listItem',
      content: [paragraphBlock(rng, 8 + Math.floor(rng() * 40), { tableRate: 0, imageRate: 0, equationRate: 0 })],
    })
  }
  return { type: 'bulletList', content }
}

function codeBlock(rng: () => number): Record<string, unknown> {
  return {
    type: 'codeBlock',
    attrs: { language: 'rust' },
    content: [{ type: 'text', text: textRun(rng, 20 + Math.floor(rng() * 40)) }],
  }
}

function blockquoteBlock(rng: () => number): Record<string, unknown> {
  return { type: 'blockquote', content: [paragraphBlock(rng, 20 + Math.floor(rng() * 50), { tableRate: 0, imageRate: 0, equationRate: 0 })] }
}

function tableBlock(rng: () => number): Record<string, unknown> {
  const rows = 2 + Math.floor(rng() * 4)
  const cols = 2 + Math.floor(rng() * 3)
  const trs: Record<string, unknown>[] = []
  for (let r = 0; r < rows; r++) {
    const tds: Record<string, unknown>[] = []
    for (let c = 0; c < cols; c++) {
      tds.push({
        type: 'tableCell',
        content: [paragraphBlock(rng, 3 + Math.floor(rng() * 12), { tableRate: 0, imageRate: 0, equationRate: 0 })],
      })
    }
    trs.push({ type: 'tableRow', content: tds })
  }
  return { type: 'table', content: trs }
}

function horizontalRule(): Record<string, unknown> {
  return { type: 'horizontalRule' }
}

export function generateCorpus(opts: CorpusOptions): Corpus {
  const rng = mulberry32(opts.seed ?? 0x5eed)
  const tableRate = opts.tableRate ?? 0.04
  const imageRate = opts.imageRate ?? 0.05
  const equationRate = opts.equationRate ?? 0.03

  const sectionCount = Math.max(1, Math.ceil(opts.targetWords / opts.wordsPerSection))
  const sections: GeneratedSection[] = []
  let grandTotal = 0

  for (let s = 0; s < sectionCount; s++) {
    const blockSpecs: Record<string, unknown>[] = []
    let sectionWords = 0

    // Each section opens with a heading so navigation and TOC have anchors.
    blockSpecs.push(headingBlock(rng, s === 0 ? 1 : 2))
    sectionWords += 8

    while (sectionWords < opts.wordsPerSection) {
      const r = rng()
      if (r < 0.06) {
        blockSpecs.push(headingBlock(rng, 2 + Math.floor(rng() * 3)))
        sectionWords += 8
      } else if (r < 0.14) {
        blockSpecs.push(listBlock(rng, 2 + Math.floor(rng() * 5)))
        sectionWords += 30
      } else if (r < 0.17) {
        blockSpecs.push(codeBlock(rng))
        sectionWords += 30
      } else if (r < 0.20) {
        blockSpecs.push(blockquoteBlock(rng))
        sectionWords += 35
      } else if (r < 0.21) {
        blockSpecs.push(horizontalRule())
      } else if (r < 0.21 + tableRate) {
        blockSpecs.push(tableBlock(rng))
        sectionWords += 60
      } else {
        const w = 40 + Math.floor(rng() * 160)
        blockSpecs.push(paragraphBlock(rng, w, { tableRate, imageRate, equationRate }))
        sectionWords += w
      }
    }

    const json = { type: 'doc', content: blockSpecs }
    sections.push({ index: s, wordCount: sectionWords, json })
    grandTotal += sectionWords
  }

  return { sections, totalWords: grandTotal }
}

/** Cheap word count for a ProseMirror doc, used to validate generation. */
export function countWords(node: unknown): number {
  let n = 0
  const walk = (x: any) => {
    if (!x || typeof x !== 'object') return
    if (Array.isArray(x)) {
      for (const i of x) walk(i)
      return
    }
    if (x.type === 'text' && typeof x.text === 'string') {
      n += x.text.trim().split(/\s+/).filter(Boolean).length
    }
    if (x.content) walk(x.content)
  }
  walk(node)
  return n
}
