/**
 * The export panel: the thing on screen for 54 seconds.
 *
 * # Why this is a module and not markup in `main.ts`
 *
 * Because the hard part is not drawing a dialog. It is the four states a long operation has —
 * running, done, cancelled, failed — and the rule that the *last* thing shown must be the truth.
 * A panel left on "Typesetting pages" after the export failed is worse than no panel, and that
 * class of bug is exactly what a state machine with one owner of the state prevents.
 *
 * # Why the markup is in `index.html` rather than built here
 *
 * So the panel's structure is the same nodes the tests assert against, on the same page, with the
 * same stylesheet. A dialog built from `createElement` would be tested against a dialog built from
 * `createElement` while the user saw a different one — which is the "two implementations that
 * agree with each other" problem this project has hit before with the wire format.
 *
 * # Why the steps are listed and not a percentage
 *
 * The phase durations are 74ms / 19,490ms / 3,211ms. A bar computed from those ratios would sit
 * at 0% through the translation, crawl through the layout, and be roughly right at the end. A
 * bar that reads as stuck is a bug even when it is accurate. Four labelled steps say *where* it
 * is, which is the question a user actually has.
 *
 * # Why Cancel stops the export rather than hiding the panel
 *
 * Because a 20-second layout is still running. Hiding the panel would leave the CPU busy, the
 * worker occupied and the file unmentioned, and the user would reasonably conclude the export had
 * failed. Cancel is honest about what it can do: it takes effect at the next phase boundary,
 * which is stated in the panel so the wait is expected rather than surprising.
 */

import type { ExportPhase } from './boot.js'
import { describeJob, type ExportJob } from './export.js'

/** The phases shown as steps, in order. The two terminal ones are not steps. */
const STEPS: Array<{ phase: ExportPhase; label: string }> = [
  { phase: 'translating', label: 'Translating' },
  { phase: 'readingAssets', label: 'Reading figures' },
  { phase: 'layout', label: 'Typesetting pages' },
  { phase: 'serializing', label: 'Writing the PDF' },
]

/** What a phase costs, stated to the user so the wait is expected. */
const PHASE_NOTES: Partial<Record<ExportPhase, string>> = {
  // Measured on a 1.33M-word document: translating 1.33M words took 74ms; layout took 19,490ms
  // and serialisation 3,211ms. Saying so up front is the difference between "this is slow" and
  // "this is broken", and it is why the steps exist rather than a spinner.
  //
  // These were 45,000ms and 9,361ms until the fonts were embedded. The same document now lays
  // out into 2,541 pages instead of 4,245, so the copy says "most of a minute" about a wait
  // that is closer to twenty seconds — which is the *right* direction to be wrong in, and is
  // still true for the two to three times longer documents this thing is for.
  layout: 'A long document takes tens of seconds here. This is Typst typesetting every page.',
  serializing: 'Writing the PDF file. The page count is already final.',
}

export interface ExportPanel {
  /** Show the panel and follow `job` until it settles. */
  open(job: ExportJob): Promise<void>
  /** Hide and tear down. */
  close(): void
  /** The elements, exposed for tests. */
  readonly elements: ExportPanelElements
}

export interface ExportPanelElements {
  modal: HTMLElement
  panel: HTMLElement
  title: HTMLElement
  detail: HTMLElement
  steps: HTMLElement
  bar: HTMLElement
  warn: HTMLElement
  cancel: HTMLButtonElement
  close: HTMLButtonElement
}

/** Find the panel's elements, failing loudly rather than returning null. */
export function panelElements(root: Document = document): ExportPanelElements {
  const need = <T extends HTMLElement>(id: string): T => {
    const el = root.getElementById(id)
    if (!el) {
      // A missing element means `index.html` and this file disagree, which is a build error and
      // not a runtime state. Throwing names the element; a null would surface three frames later
      // as "cannot read property of null".
      throw new Error(`the export panel markup is missing #${id}; index.html and export-panel.ts disagree`)
    }
    return el as T
  }
  return {
    modal: need('export-modal'),
    panel: need('export-panel'),
    title: need('export-title'),
    detail: need('export-detail'),
    steps: need('export-steps'),
    bar: need('export-bar'),
    warn: need('export-warn'),
    cancel: need('export-cancel') as HTMLButtonElement,
    close: need('export-close') as HTMLButtonElement,
  }
}

/**
 * Build the panel's behaviour.
 *
 * `onClosed` is called when the panel goes away for any reason, so a caller can restore whatever
 * the panel suspended. It is a parameter rather than a hardcoded call because "what does the panel
 * take away" is the caller's knowledge: an export suspends nothing here, but a future
 * document-wide operation would.
 */
export function createExportPanel(options: {
  onCancel: (job: ExportJob) => void
  onClosed?: () => void
  root?: Document
}): ExportPanel {
  const elements = panelElements(options.root)

  // The step list is built once, on open, and then only its `data-state` changes. Rebuilding it on
  // every progress report would replace the nodes a screen reader is holding a reference to, and
  // would lose the focus ring if the user was tabbing through.
  function renderSteps(phase: ExportPhase): void {
    const index = STEPS.findIndex(step => step.phase === phase)
    for (const [i, step] of STEPS.entries()) {
      const li = elements.steps.children[i] as HTMLElement | undefined
      if (!li) continue
      li.dataset.phase = step.phase
      // `active` on the current step, `done` on those before it. A step after the current one is
      // left with no state at all, which renders as the empty circle — a step that has not
      // happened yet must not look done.
      li.dataset.state = index < 0 ? '' : i < index ? 'done' : i === index ? 'active' : ''
    }
    const segments = elements.bar.children
    for (let i = 0; i < segments.length; i++) {
      const span = segments[i] as HTMLElement
      span.dataset.state = index < 0 ? '' : i < index ? 'done' : i === index ? 'active' : ''
    }
  }

  function show(phase: ExportPhase, message: string, seconds: number): void {
    elements.title.textContent = 'Exporting PDF'
    elements.detail.textContent = message
    renderSteps(phase)

    const note = PHASE_NOTES[phase]
    if (note && seconds >= 1) {
      elements.warn.hidden = false
      elements.warn.textContent = note
    } else {
      elements.warn.hidden = true
    }

    // Cancel is offered only while something can still be cancelled. Offering it after the export
    // finished would be a button that does nothing, which is worse than no button.
    elements.cancel.disabled = false
    elements.close.hidden = true
  }

  function settle(job: ExportJob): void {
    const state = job.state
    switch (state.kind) {
      case 'running':
        break
      case 'done': {
        elements.title.textContent = 'Export complete'
        elements.detail.textContent = describeJob(job)
        renderSteps('done')
        elements.warn.hidden = true
        // Both buttons become "dismiss", because the operation is over and the only thing left
        // to do is close the panel.
        elements.cancel.hidden = true
        elements.close.hidden = false
        elements.close.focus()
        break
      }
      case 'cancelled': {
        elements.title.textContent = 'Export cancelled'
        elements.detail.textContent = describeJob(job)
        elements.warn.hidden = false
        elements.warn.textContent =
          'The export stopped. Typesetting runs in a separate process, so it stops where it ' +
          'is rather than at a step boundary.'
        elements.cancel.hidden = true
        elements.close.hidden = false
        elements.close.focus()
        break
      }
      case 'failed': {
        elements.title.textContent = 'Export failed'
        elements.detail.textContent = describeJob(job)
        elements.warn.hidden = true
        elements.cancel.hidden = true
        elements.close.hidden = false
        elements.close.focus()
        break
      }
    }
  }

  let current: ExportJob | null = null
  let detachEscape: (() => void) | null = null

  function close(): void {
    detachEscape?.()
    detachEscape = null
    elements.modal.hidden = true
    current = null
    options.onClosed?.()
  }

  // Escape closes the panel. A *running* export keeps going, and the panel says so, because
  // closing the panel is not the same as cancelling the export and conflating them is how a user
  // ends up with a document exporting invisibly.
  function onKeydown(event: KeyboardEvent): void {
    if (event.key !== 'Escape') return
    event.preventDefault()
    close()
  }

  return {
    elements,

    async open(job: ExportJob): Promise<void> {
      current = job
      // Build the step list fresh each open, and clear any state from the previous export -- a
      // panel that says "Typesetting pages" from a finished job while the next one is
      // translating is exactly the stale-state bug this module exists to prevent.
      elements.steps.replaceChildren(
        ...STEPS.map(step => {
          const li = document.createElement('li')
          li.textContent = step.label
          li.dataset.phase = step.phase
          return li
        }),
      )
      elements.cancel.hidden = false
      elements.close.hidden = true

      const onProgress = (): void => {
        if (current !== job) return
        const state = job.state
        if (state.kind === 'running') {
          show(state.phase, state.message, Math.floor(state.elapsedMs / 1000))
        } else {
          settle(job)
        }
      }

      const onClick = (): void => {
        if (current !== job) return
        void options.onCancel(job)
        // Disabled immediately, so a second click cannot queue a second cancel. The backend
        // reports whether a job was actually running, and a cancel that arrives after the
        // export finished is a no-op there -- but a button that stays enabled invites the
        // user to keep clicking.
        elements.cancel.disabled = true
      }
      elements.cancel.addEventListener('click', onClick)
      elements.close.addEventListener('click', close)
      document.addEventListener('keydown', onKeydown)
      detachEscape = () => {
        elements.cancel.removeEventListener('click', onClick)
        elements.close.removeEventListener('click', close)
        document.removeEventListener('keydown', onKeydown)
      }

      elements.modal.hidden = false
      show('translating', 'starting', 0)
      // Focus the panel so a keyboard user is inside the dialog rather than behind it, and so
      // Tab cycles within it. `aria-modal` tells assistive technology the rest is inert; this
      // makes it true for a keyboard.
      elements.panel.focus()

      // Poll for terminal state rather than subscribing to a second stream. The job already has
      // an event-driven path for the *backend's* reports; this is a UI loop that must also
      // notice the promise settling, and one loop is simpler than reconciling two sources.
      // 120ms is short enough to feel immediate and long enough that a 54-second export does
      // 450 wakeups rather than 540,000.
      for (;;) {
        await new Promise(resolve => setTimeout(resolve, 120))
        if (current !== job) return
        if (job.state.kind !== 'running') {
          onProgress()
          return
        }
        onProgress()
      }
    },

    close,
  }
}
