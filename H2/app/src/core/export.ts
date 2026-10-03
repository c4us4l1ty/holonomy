/**
 * Driving an export from the frontend: one job at a time, watched, and stoppable.
 *
 * # Why this is a module and not three functions in `main.ts`
 *
 * Because the awkward part of an export is not calling the command. It is that a 54-second
 * operation has *four phases of wildly unequal length* — 35ms translating, 45,000ms laying out,
 * 9,361ms serialising — and a UI that shows nothing for that time is indistinguishable from a UI
 * that has hung. So the state machine, the listener lifetime and the cancel path all have to be
 * right together, and they are far easier to test together than to test through a window.
 *
 * # Why the job id is generated here and not by the backend
 *
 * So a caller can listen and cancel under a name it already knows, and so a second export does not
 * overwrite the first's status line. A server-generated id would be tidier and would leave the
 * frontend unable to predict the name it must act on.
 *
 * # Why there is no percentage
 *
 * The phase ratios are measured (35 / 45,000 / 9,361 ms) and a bar computed from them would be
 * wrong by three orders of magnitude in the first phase and roughly right in the third. It would
 * read as stuck, then jump. Phase plus elapsed is the honest version, and it is what the backend
 * reports. See `holonomy-shell/src/export/progress.rs`.
 */

import type { ExportPhase, ExportProgress, ExportReply } from './boot.js'

/** The event the backend emits. Mirrors `EXPORT_PROGRESS_EVENT` in `lib.rs`. */
export const EXPORT_PROGRESS_EVENT = 'holo://export-progress'

/**
 * The prefix on a cancelled export's error. Mirrors `EXPORT_CANCELLED_PREFIX` in `lib.rs`.
 *
 * # Why a prefix and not an error code
 *
 * Because Tauri commands return `Result<T, String>` and a string is what arrives. A prefix is
 * matched rather than parsed, and the two constants are asserted equal by a test so they cannot
 * drift — which is the failure this pairing is prone to, having already drifted twice for the
 * `u128`-to-`bigint` annotations in the same file.
 */
export const CANCELLED_PREFIX = 'holonomy: export cancelled'

/** Whether a rejection is a cancellation rather than a failure. */
export function isCancelled(message: string): boolean {
  return message.startsWith(CANCELLED_PREFIX)
}

/** How a job ended. */
export type JobState =
  | { readonly kind: 'running'; readonly phase: ExportPhase; readonly elapsedMs: number; readonly message: string }
  | { readonly kind: 'done'; readonly reply: ExportReply }
  | { readonly kind: 'cancelled'; readonly elapsedMs: number }
  | { readonly kind: 'failed'; readonly message: string }

/** What a caller supplies, so this module does not import the bridge. */
export interface ExportBackend {
  /** Start an export. Rejects with a string; a cancellation is one of those strings. */
  start(jobId: string, path?: string): Promise<ExportReply>
  /** Ask the backend to stop. Resolves to whether a job was actually running. */
  cancel(jobId: string): Promise<boolean>
  /** Subscribe to progress. Returns an unsubscribe function. */
  onProgress(handler: (progress: ExportProgress) => void): Promise<() => void>
}

/** A running or finished export, as the UI sees it. */
export interface ExportJob {
  readonly jobId: string
  readonly state: JobState
  /** Fraction complete, or `null` when it cannot be known. See the note on percentages above. */
  readonly fraction: number | null
  /** Ask the backend to stop. Does nothing if the job is not running. */
  cancel(): Promise<boolean>
  /** A promise for the terminal state. Rejects only if the *caller* asked for the throwing form. */
  settled(): Promise<JobState>
}

/**
 * The fraction to show, and why it is coarse.
 *
 * Four phases with measured weights, so the bar moves in four steps rather than smoothly. That is
 * a deliberate trade: a bar that sits at 0% for 45 seconds and then jumps to 100% is *accurate*
 * and reads as broken, whereas a four-step bar that reaches "typesetting pages" and sits there is
 * both accurate and informative — because the step itself is the message.
 *
 * The weights are the measured ratios, rounded to something a person can reason about, and they
 * are `null` rather than a guess for the last phase because a bar at 100% while 9 seconds of
 * serialisation remain is the one thing a progress indicator must not do.
 */
const PHASE_PROGRESS: Record<ExportPhase, number | null> = {
  translating: 0.02,
  readingAssets: 0.03,
  layout: 0.35,
  serializing: 0.9,
  done: 1,
  cancelled: 1,
  failed: 1,
}

/** Monotonic job ids, so two exports in one session never share a name. */
let nextJob = 0

/** A job id nobody else is using. */
export function newJobId(prefix = 'export'): string {
  nextJob += 1
  return `${prefix}-${nextJob}-${Date.now().toString(36)}`
}

/**
 * Start an export and watch it.
 *
 * # The listener is unsubscribed on every exit path
 *
 * Because the event is process-wide: a subscription left open after a job ends would keep firing
 * for the *next* job, and the status line would show two jobs' phases interleaved. The `finally`
 * covers the success, the cancellation and the failure, which is why the unsubscribe is there and
 * not in the happy path.
 *
 * # The progress handler filters by job id
 *
 * Belt and braces with the unsubscribe. Two exports can genuinely overlap — the window is not
 * modal over the document — and a report from one must not be rendered as the other's phase. The
 * job id is on every report for exactly this.
 */
export async function startExport(backend: ExportBackend, options: { path?: string } = {}): Promise<ExportJob> {
  const jobId = newJobId()
  let state: JobState = {
    kind: 'running',
    phase: 'translating',
    elapsedMs: 0,
    message: 'starting',
  }
  let settle: (value: JobState) => void = () => {}
  let fail: (reason: Error) => void = () => {}
  const settled = new Promise<JobState>((resolve, reject) => {
    settle = resolve
    fail = reject
  })
  // Nothing awaits `settled` until the caller does, and a rejection with no handler yet is not a
  // problem in JavaScript -- it becomes one only if it is still unhandled when the microtask
  // queue drains. Attaching a no-op catch immediately and returning the original promise keeps
  // both properties.
  settled.catch(() => {})

  const listeners = new Set<() => void>()
  const notify = (): void => {
    for (const listener of [...listeners]) listener()
  }

  const unsubscribe = await backend.onProgress(progress => {
    if (progress.job_id !== jobId) return
    state = {
      kind: 'running',
      phase: progress.phase,
      elapsedMs: progress.elapsed_ms,
      message: progress.message,
    }
    notify()
  })

  const job: ExportJob = {
    jobId,
    get state() {
      return state
    },
    get fraction() {
      return state.kind === 'running' ? PHASE_PROGRESS[state.phase] ?? null : state.kind === 'done' ? 1 : null
    },
    async cancel() {
      if (state.kind !== 'running') return false
      return backend.cancel(jobId)
    },
    settled: () => settled,
  }

  void (async () => {
    try {
      const reply = await backend.start(jobId, options.path)
      state = { kind: 'done', reply }
      settle(state)
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error)
      if (isCancelled(message)) {
        state = { kind: 'cancelled', elapsedMs: 0 }
        settle(state)
      } else {
        state = { kind: 'failed', message }
        fail(new Error(message))
      }
    } finally {
      unsubscribe()
      notify()
    }
  })()

  return job
}

/** Whether a job is still running. */
export function isRunning(job: ExportJob): boolean {
  return job.state.kind === 'running'
}

/**
 * One line describing a job, for a status bar.
 *
 * Exported rather than inlined into a component so the wording is testable without a DOM, and so
 * the *same* sentence appears in the status bar, the modal and any future notification. A phrase
 * that differs between two surfaces is how a user gets told two different things about one job.
 */
export function describeJob(job: ExportJob): string {
  const state = job.state
  switch (state.kind) {
    case 'running': {
      const seconds = Math.floor(state.elapsedMs / 1000)
      const timing = seconds >= 1 ? ` (${seconds}s)` : ''
      return `${state.message}${timing}`
    }
    case 'done': {
      const { pages, elapsed_ms, layout_ms, translate_ms } = state.reply
      // Both numbers, because they differ by three orders of magnitude and showing only the
      // total would make a translator regression invisible.
      return `Exported ${pages} page(s) in ${Math.round(elapsed_ms / 100) / 10}s — ${Math.round(translate_ms)}ms translating, ${Math.round(layout_ms / 1000)}s typesetting`
    }
    case 'cancelled':
      return 'Export cancelled'
    case 'failed':
      return `Export failed: ${state.message}`
  }
}
