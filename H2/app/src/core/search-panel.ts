/**
 * The find panel: a query box, a list of matching sections, and a way to go to one.
 *
 * # Why this exists rather than the browser's own find
 *
 * Native find searches the mounted DOM, and a Holonomy document mounts 3 to 5 sections out
 * of 1,300. On the 667-section corpus that is well under 1% of the text — so native find
 * reports "0 results" for a term that appears on page 400, and a user concludes the text is
 * not there. `2000.md` §6 puts search on FTS5 for exactly this reason, and
 * `holonomy-core::Store::search` is that FTS5 query.
 *
 * The panel is *not* a modal and there is no scrim. Search is somewhere you go and come
 * back from; the text you are searching stays visible and clickable while you search it,
 * which a `position: fixed; inset: 0` overlay would prevent.
 *
 * # Why results are sections, not occurrences
 *
 * The backend returns one row per section, because that is what an index over
 * per-section text can honestly produce and because "which section, and what does it say"
 * is what a person needs. Two occurrences in one section are one result with a snippet that
 * may not contain the second one; the alternative is a section-level API pretending to be
 * occurrence-level, which is the shape of bug that makes a find bar untrustworthy. The
 * panel says so in its summary rather than implying a completeness it does not have.
 *
 * # Debouncing
 *
 * 120ms after the last keystroke. Two reasons rather than one: below ~120ms a search fires
 * while a word is half-typed and the results visibly jump as the query grows; above it, a
 * fast typist notices the lag. FTS5 over 1.33M words is a few milliseconds, so the delay
 * is the debounce and not the query.
 */

import type { SearchHit, SearchResponse } from './generated-bridge.js'

export interface SearchPanelElements {
  modal: HTMLElement
  panel: HTMLElement
  input: HTMLInputElement
  summary: HTMLElement
  results: HTMLElement
}

/** How long after the last keystroke a query runs. */
export const SEARCH_DEBOUNCE_MS = 120

/**
 * Find the panel's elements, failing loudly rather than returning null.
 *
 * Same shape as `export-panel.ts` and for the same reason: a missing element means
 * `index.html` and this file disagree, which is a build error and not a runtime state.
 */
export function searchPanelElements(root: Document = document): SearchPanelElements {
  const need = <T extends HTMLElement>(id: string): T => {
    const el = root.getElementById(id)
    if (!el) {
      throw new Error(`the search panel markup is missing #${id}; index.html and search-panel.ts disagree`)
    }
    return el as T
  }
  return {
    modal: need('search-modal'),
    panel: need('search-panel'),
    input: need('search-input') as HTMLInputElement,
    summary: need('search-summary'),
    results: need('search-results'),
  }
}

export interface SearchPanel {
  /** Show the panel and focus the box, selecting whatever is already there. */
  open(): void
  /** Hide the panel and drop its state. */
  close(): void
  /** Whether the panel is on screen. */
  readonly isOpen: boolean
  /**
   * Run `query` and render, bypassing the debounce.
   *
   * Exposed because the tests need a deterministic path to results, and because "type and
   * wait 120ms" is a test that is really testing the test's sleep.
   */
  queryNow(query: string): Promise<void>
  readonly elements: SearchPanelElements
}

export function createSearchPanel(options: {
  /** Run a query against the backend. */
  search: (query: string) => Promise<SearchResponse>
  /**
   * Take the user to a hit.
   *
   * The query travels with the hit because placing the caret needs it. The snippet
   * carries `<b>` markers that are not in the document text, so the caller cannot derive
   * the search term from what it was given, and making it reach into the panel's input
   * would couple the callback to the panel's internals.
   */
  onNavigate: (hit: SearchHit, query: string) => void | Promise<void>
  root?: Document
}): SearchPanel {
  const elements = searchPanelElements(options.root)

  let hits: SearchHit[] = []
  let selected = -1
  // `window.setTimeout` rather than the bare global: this is browser code, the DOM
  // overload is the one that applies, and it is what makes the timer's type `number`
  // instead of whatever ambient timer type the project's Node typings introduce. The
  // alternative -- inferring the type from the global -- couples this module to whatever
  // that resolves to.
  let timer: number | null = null
  let detach: (() => void) | null = null
  /** Bumped per query so a slow response for an old query cannot overwrite a newer one. */
  let generation = 0
  /**
   * The query the current hit list came from.
   *
   * Read by the click and arrow handlers, which fire outside `run` and so have no other
   * way to know what the user typed. Set in `run` *after* the generation check, so a hit
   * list is never labelled with a query that has already been superseded.
   */
  let lastQuery = ''

  /**
   * Render the snippet, which arrives with `<b>` markers from the backend.
   *
   * Built by walking the string and splitting on those markers, with every other segment
   * becoming a **text node**. That is the whole safety argument and the reason this does
   * not use `innerHTML`: the only elements it can ever produce are the `<b>` tags it
   * creates itself. A document containing `<script>` renders as text that reads
   * `<script>` rather than as an element that runs.
   *
   * It matters that the backend does not escape its output. `snippet_around` wraps the
   * matched run in literal `<b>` and passes the surrounding document text through
   * untouched, so a document whose text contains markup arrives here containing that
   * markup, and this function is where it stops being markup.
   */
  function renderSnippet(snippet: string): DocumentFragment {
    const frag = document.createDocumentFragment()
    let bold: HTMLElement | null = null
    for (const part of snippet.split(/(<\/?b>)/)) {
      if (part === '<b>') {
        bold = document.createElement('b')
        frag.appendChild(bold)
      } else if (part === '</b>') {
        bold = null
      } else if (part) {
        if (bold) bold.appendChild(document.createTextNode(part))
        else frag.appendChild(document.createTextNode(part))
      }
    }
    return frag
  }

  function summaryFor(total: number, query: string): string {
    const trimmed = query.trim()
    if (!trimmed) return ''
    if (total === 0) return `No sections contain “${trimmed}”`
    const noun = total === 1 ? 'section' : 'sections'
    const shown = Math.min(hits.length, total)
    return shown < total
      ? `${total} ${noun} contain “${trimmed}”, showing the first ${shown}`
      : `${total} ${noun} contain “${trimmed}”`
  }

  function render(response: SearchResponse, query: string): void {
    hits = response.hits
    elements.summary.textContent = summaryFor(response.total, query)

    const items = hits.map((hit, i) => {
      const li = document.createElement('li')
      li.setAttribute('aria-selected', String(i === selected))
      li.dataset.sectionId = hit.section_id

      const button = document.createElement('button')
      button.type = 'button'
      button.dataset.index = String(i)

      // A short section label above the snippet, so a list of ten hits in the same
      // section is still readable. `Section 12` rather than the ULID: nobody recognises
      // a ULID and the panel already shows enough identifiers.
      const label = document.createElement('span')
      label.className = 'search-label'
      label.textContent = `Section ${i + 1}`
      button.appendChild(label)

      const snippet = document.createElement('span')
      snippet.className = 'search-snippet'
      snippet.appendChild(renderSnippet(hit.snippet))
      button.appendChild(snippet)

      button.addEventListener('click', () => {
        // The hit is captured by value rather than looked up again. `hits` is replaced
        // wholesale on the next query, so a click that arrives after a newer query has
        // landed must navigate to the row the user actually clicked, not to whatever now
        // occupies that index.
        select(i)
        void options.onNavigate(hit, lastQuery)
      })

      li.appendChild(button)
      return li
    })

    elements.results.replaceChildren(...items)
  }

  function select(index: number): void {
    selected = index
    for (const [i, child] of Array.from(elements.results.children).entries()) {
      child.setAttribute('aria-selected', String(i === selected))
    }
  }

  /** Move the selection by `delta`, clamped, and navigate if it lands on a hit. */
  function step(delta: number): void {
    if (hits.length === 0) return
    const next = Math.min(Math.max(selected + delta, 0), hits.length - 1)
    const hit = hits[next]
    if (!hit) return
    select(next)
    void options.onNavigate(hit, lastQuery)
  }

  async function run(query: string): Promise<void> {
    const mine = ++generation
    const trimmed = query.trim()
    if (!trimmed) {
      hits = []
      selected = -1
      lastQuery = ''
      elements.summary.textContent = ''
      elements.results.replaceChildren()
      return
    }
    try {
      const response = await options.search(trimmed)
      // A response for a query the user has already typed past. Rendering it would make
      // the list jump back to an older query after a newer one has already landed, which
      // is the single most disorienting thing a search box can do.
      if (mine !== generation) return
      const first = response.hits[0]
      selected = first ? 0 : -1
      lastQuery = trimmed
      render(response, trimmed)
      // Navigating to the first hit automatically is what makes this a find bar rather
      // than a result list: the user typed a word and wants to be *at* it.
      if (first) await options.onNavigate(first, trimmed)
    } catch (e: any) {
      if (mine !== generation) return
      hits = []
      elements.summary.textContent = `Search failed: ${e?.message ?? String(e)}`
      elements.results.replaceChildren()
    }
  }

  function close(): void {
    detach?.()
    detach = null
    if (timer !== null) window.clearTimeout(timer)
    timer = null
    elements.modal.hidden = true
    hits = []
    selected = -1
    elements.results.replaceChildren()
    elements.summary.textContent = ''
    elements.input.value = ''
  }

  function onKeydown(event: KeyboardEvent): void {
    if (event.key === 'Escape') {
      event.preventDefault()
      close()
      return
    }
    if (event.key === 'Enter') {
      event.preventDefault()
      step(event.shiftKey ? -1 : 1)
      return
    }
    // Arrow keys move between hits rather than moving the caret, which is what every find
    // bar does and what makes Enter/Shift-Enter unnecessary. Only when the caret is not
    // being used for text, which the input's own arrow behaviour covers for the text; the
    // result list is what these drive.
    if (event.key === 'ArrowDown' && hits.length > 0) {
      event.preventDefault()
      step(1)
    } else if (event.key === 'ArrowUp' && hits.length > 0) {
      event.preventDefault()
      step(-1)
    }
  }

  return {
    elements,

    get isOpen() {
      return !elements.modal.hidden
    },

    queryNow: query => run(query),

    close,

    open(): void {
      elements.modal.hidden = false
      const onInput = (): void => {
        if (timer !== null) window.clearTimeout(timer)
        timer = window.setTimeout(() => {
          timer = null
          void run(elements.input.value)
        }, SEARCH_DEBOUNCE_MS)
      }
      elements.input.addEventListener('input', onInput)
      elements.input.addEventListener('keydown', onKeydown)
      detach = () => {
        elements.input.removeEventListener('input', onInput)
        elements.input.removeEventListener('keydown', onKeydown)
      }
      // Select rather than place at the end: reopening the panel should let a user retype
      // the previous query immediately, which is the common case.
      elements.input.focus()
      elements.input.select()
    },
  }
}