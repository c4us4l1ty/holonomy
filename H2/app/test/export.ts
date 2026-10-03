/**
 * The export client: one job, watched, stoppable.
 *
 * # Why the backend is faked rather than absent
 *
 * Because the awkward parts of an export are all in the *client's* handling of it, and none of
 * them need Rust to be wrong. A listener that outlives its job, a cancel that lands on the wrong
 * one, a panel left showing "Typesetting pages" after a failure, a rejected promise with no
 * handler — every one of those is reachable with a fake backend, and none is reachable by
 * watching a real export succeed once.
 *
 * The fake is a hand-written object implementing `ExportBackend`, not a mock framework: a mock
 * that asserts call order would pass while the module ignored the calls.
 *
 * Run: node --experimental-strip-types test/export.ts
 */

import {
  CANCELLED_PREFIX,
  describeJob,
  isCancelled,
  newJobId,
  startExport,
  type ExportBackend,
  type ExportJob,
} from '../src/core/export.ts'
import type { ExportPhase, ExportProgress, ExportReply } from '../src/core/boot.ts'

let passed = 0
let failed = 0
const failures: string[] = []

function ok(cond: unknown, msg: string): asserts cond {
  if (!cond) throw new Error(msg)
}

const pending: Array<Promise<unknown>> = []

function test(name: string, fn: () => unknown | Promise<unknown>): void {
  pending.push(
    Promise.resolve()
      .then(fn)
      .then(detail => {
        passed++
        console.log(`PASS  ${name}${detail !== undefined ? `  ${JSON.stringify(detail)}` : ''}`)
      })
      .catch((e: any) => {
        failed++
        failures.push(name)
        console.log(`FAIL  ${name}\n        ${e.message}`)
      }),
  )
}

/** A reply that satisfies the type, with the counts a status line would show. */
function reply(pages = 12): ExportReply {
  return {
    document_id: 'doc-1',
    pdf: new Uint8Array([0x25, 0x50, 0x44, 0x46]),
    pages,
    elapsed_ms: 54_764,
    translate_ms: 35,
    layout_ms: 45_000,
    serialize_ms: 9_361,
    unknown_types: [],
    lossy_marks: [],
    warnings: [],
    lossy_summary: null,
  }
}

/** A fake backend whose lifecycle the test drives. */
function fakeBackend() {
  const handlers = new Set<(progress: ExportProgress) => void>()
  let unsubscribed = 0
  const cancels: string[] = []
  /**
   * One resolver per `start` call, in order.
   *
   * A single slot was the first version, and it silently could not express two overlapping jobs:
   * the second `start` overwrote the first's resolver, so `finish()` settled only the second and
   * the first job's promise was never settled at all. Node reported it as an unsettled top-level
   * await, which is a confusing way to learn that the *test* was the broken thing.
   */
  const pendingStarts: Array<{
    resolve: (value: ExportReply) => void
    reject: (error: Error) => void
  }> = []
  let startCalls = 0

  const backend: ExportBackend = {
    async start(): Promise<ExportReply> {
      startCalls++
      return new Promise<ExportReply>((resolve, reject) => {
        pendingStarts.push({ resolve, reject })
      })
    },
    async cancel(jobId: string): Promise<boolean> {
      cancels.push(jobId)
      return true
    },
    async onProgress(handler): Promise<() => void> {
      handlers.add(handler)
      return () => {
        handlers.delete(handler)
        unsubscribed++
      }
    },
  }

  return {
    backend,
    /** Send a progress report, as the backend's event would. */
    report(jobId: string, phase: ExportPhase, elapsedMs = 0, message: string = String(phase)): void {
      const progress: ExportProgress = { job_id: jobId, phase, elapsed_ms: elapsedMs, message }
      for (const handler of [...handlers]) handler(progress)
    },
    handlerCount: () => handlers.size,
    unsubscribeCount: () => unsubscribed,
    cancels,
    startCount: () => startCalls,
    /** Resolve the oldest unsettled `start`. */
    finish(value = reply()): void {
      const next = pendingStarts.shift()
      if (!next) throw new Error('finish() with no export running; the test asked for the wrong thing')
      next.resolve(value)
    },
    /** Reject the oldest unsettled `start`, for the failure and cancellation paths. */
    abort(message: string): void {
      const next = pendingStarts.shift()
      if (!next) throw new Error('abort() with no export running; the test asked for the wrong thing')
      next.reject(new Error(message))
    },
    /** How many `start` calls have not settled. Zero at the end of every test. */
    outstanding: () => pendingStarts.length,
  }
}

/** Let queued microtasks and one timer run, so the client has observed whatever just happened. */
async function settle(times = 3): Promise<void> {
  for (let i = 0; i < times; i++) await Promise.resolve()
}

console.log('export: one job, watched, stoppable')
console.log('='.repeat(72))

test('a cancellation is recognised by its prefix and nothing else is', () => {
  ok(isCancelled(`${CANCELLED_PREFIX}: stopped after 120ms`), 'the prefix plus a detail is a cancellation')
  ok(isCancelled(CANCELLED_PREFIX), 'and the bare prefix is too')
  // A genuine failure must not be mistaken for a cancellation: the two need opposite handling,
  // so a loose match would show a dialog for a document that failed to typeset.
  ok(!isCancelled('could not export the document to PDF: unknown font family'), 'a failure is not a cancellation')
  // A longer string that legitimately starts with the prefix *is* a cancellation -- the backend
  // appends the elapsed time, so matching is on the prefix and not on equality. The near-miss that
  // must not match is a different prefix.
  ok(isCancelled(`${CANCELLED_PREFIX}: stopped after 120ms`), 'a longer message is still one')
  ok(!isCancelled('holonomy: the export was cancelled by the user'), 'a different prefix is not a cancellation')
  return { prefix: CANCELLED_PREFIX }
})

test('job ids are unique and carry the prefix', () => {
  const ids = new Set(Array.from({ length: 50 }, () => newJobId()))
  ok(ids.size === 50, `expected 50 distinct ids, got ${ids.size}`)
  ok([...ids].every(id => id.startsWith('export-')), 'and all should carry the prefix')
  ok(newJobId('backup').startsWith('backup-'), 'a different prefix is honoured, so a backup is not an export')
  return { unique: ids.size }
})

test('a running job reports the phase the backend sent', async () => {
  const env = fakeBackend()
  const job = await startExport(env.backend)

  ok(job.state.kind === 'running', 'a new job is running')
  env.report(job.jobId, 'layout', 45_000, 'Typesetting pages')
  await settle()

  const state = job.state
  ok(state.kind === 'running', 'and still running')
  if (state.kind !== 'running') throw new Error('unreachable')
  ok(state.phase === 'layout', `expected the layout phase, got ${state.phase}`)
  ok(state.elapsedMs === 45_000, `the elapsed time should come from the report: ${state.elapsedMs}`)
  ok(state.message === 'Typesetting pages', 'and so should the message')

  const runningFraction = job.fraction
  env.finish()
  await job.settled()
  // Read *before* settling: afterwards the job is `done` and the fraction is 1, which is what the
  // first version of this assertion reported and briefly looked like a bug in the client.
  ok(runningFraction === 0.35, `the layout phase should be 35% of the bar, got ${runningFraction}`)
  return { phase: state.phase, fraction: runningFraction }
})

test("a report for another job's id is ignored", async () => {
  // Two exports can overlap — the panel is an overlay, not a modal over the document — so a report
  // from one must not be rendered as the other's phase. Without the filter, the status line shows
  // whichever event arrived last.
  const env = fakeBackend()
  const first = await startExport(env.backend)
  const second = await startExport(env.backend)
  ok(first.jobId !== second.jobId, 'two jobs should have different ids')

  env.report(second.jobId, 'serializing', 9_000)
  await settle()

  const firstState = first.state
  const secondState = second.state
  ok(firstState.kind === 'running' && secondState.kind === 'running', 'both still running')
  if (firstState.kind !== 'running' || secondState.kind !== 'running') throw new Error('unreachable')
  ok(
    firstState.phase !== 'serializing',
    `the first job took the second job's phase: ${firstState.phase}`
  )
  ok(secondState.phase === 'serializing', 'while the second job did take its own')

  env.finish()
  env.finish()
  await Promise.all([first.settled(), second.settled()])
  ok(env.outstanding() === 0, 'every export the test started should have settled')
  return { first: firstState.phase, second: secondState.phase }
})

test('the listener is unsubscribed when the job ends, however it ends', async () => {
  // The event is process-wide. A subscription left open after a job ends keeps firing for the
  // *next* job, and the status line then shows two jobs' phases interleaved.
  for (const [how, end] of [
    ['success', (env: ReturnType<typeof fakeBackend>) => env.finish()],
    [
      'failure',
      (env: ReturnType<typeof fakeBackend>) => env.abort('could not typeset "X": unknown font family'),
    ],
    ['cancellation', (env: ReturnType<typeof fakeBackend>) => env.abort(`${CANCELLED_PREFIX}: stopped`)],
  ] as const) {
    const env = fakeBackend()
    const job = await startExport(env.backend)
    ok(env.handlerCount() === 1, `the ${how} job should have one listener while running`)

    end(env)
    try {
      await job.settled()
    } catch {
      // The failure case rejects; the test below asserts on the state instead.
    }
    await settle()

    ok(env.handlerCount() === 0, `the listener should be gone after ${how}, got ${env.handlerCount()}`)
    ok(env.unsubscribeCount() === 1, `and unsubscribed exactly once for ${how}, got ${env.unsubscribeCount()}`)
  }
  return { cases: 3 }
})

test('a failure rejects the settled promise and names the reason', async () => {
  const env = fakeBackend()
  const job = await startExport(env.backend)
  env.abort('could not typeset "Chapter": unknown variable: frak')
  let rejected: string | null = null
  try {
    await job.settled()
  } catch (e: any) {
    rejected = e.message
  }
  await settle()

  ok(rejected !== null, 'a failure should reject')
  ok(
    rejected!.includes('unknown variable: frak'),
    `and the reason should survive to the caller: ${rejected}`
  )
  ok(job.state.kind === 'failed', `expected the failed state, got ${job.state.kind}`)
  // The unhandled-rejection hazard: `settled()` is only awaited by a caller who asks, so the
  // promise must not warn before then. It is returned from an async function, so nothing has
  // touched it yet.
  return { rejected: rejected!.slice(0, 40) }
})

test('a cancellation settles rather than rejects, because the two need opposite handling', async () => {
  // A rejection would send a caller that only catches errors down an error path for something the
  // user asked for. The state says `cancelled` and the promise resolves, so `await job.settled()`
  // followed by a state check is the whole pattern.
  const env = fakeBackend()
  const job = await startExport(env.backend)
  env.abort(`${CANCELLED_PREFIX}: stopped after 120ms`)
  const state = await job.settled()
  ok(state.kind === 'cancelled', `expected cancelled, got ${state.kind}`)
  ok(job.state.kind === 'cancelled', 'and the job agrees')
  return { state: state.kind }
})

test('a cancel is forwarded under the right job id, and is refused once finished', async () => {
  const env = fakeBackend()
  const job = await startExport(env.backend)

  ok(await job.cancel(), 'cancelling a running job should report that it was running')
  ok(env.cancels.length === 1, `expected one cancel, got ${env.cancels.length}`)
  ok(env.cancels[0] === job.jobId, `the cancel should name this job: ${env.cancels[0]}`)

  env.finish()
  await job.settled()
  // A cancel after the fact is a no-op rather than an error: a user clicking Cancel as the export
  // finishes is not a failure, and saying so would be wrong.
  ok((await job.cancel()) === false, 'cancelling a finished job should report that nothing was running')
  ok(env.cancels.length === 1, 'and should not have reached the backend a second time')
  return { cancels: env.cancels.length }
})

test('the fraction is coarse and never claims to be finished early', async () => {
  // The bar is four steps rather than smooth, because the phases are 35ms / 45,000 / 9,361 and a
  // linear bar would read as stuck. What it must never do is reach 100% while work remains: a
  // progress indicator that says "done" and then waits 9 seconds is the one thing it must not be.
  const env = fakeBackend()
  const job = await startExport(env.backend)
  const seen: Array<[string, number | null]> = []
  for (const phase of ['translating', 'readingAssets', 'layout', 'serializing'] as ExportPhase[]) {
    env.report(job.jobId, phase, 1000)
    await settle()
    const state = job.state
    if (state.kind !== 'running') throw new Error(`unexpectedly ${state.kind} at ${phase}`)
    seen.push([phase, job.fraction])
  }
  for (const [phase, fraction] of seen) {
    ok(fraction !== null, `${phase} should have a fraction`)
    ok(fraction! < 1, `${phase} must not claim to be complete; got ${fraction}`)
  }
  // Monotone, because a bar that counts up then down reads as a bug even when both are true.
  for (let i = 1; i < seen.length; i++) {
    ok(seen[i]![1]! >= seen[i - 1]![1]!, `the fraction went backwards at ${seen[i]![0]}: ${seen[i - 1]![1]} -> ${seen[i]![1]}`)
  }
  env.finish()
  await job.settled()
  ok(job.fraction === 1, 'a finished job is complete')
  // A cancelled or failed one has no fraction: there is nothing to be a fraction *of*, and a bar
  // sitting at 35% on a job that is over would invite the user to wait for the rest of it.
  const cancelled = await startExport(env.backend)
  env.abort(`${CANCELLED_PREFIX}: stopped`)
  await cancelled.settled()
  ok(cancelled.fraction === null, 'a cancelled job has no fraction')
  return { steps: seen.map(([phase, fraction]) => `${phase}=${fraction}`) }
})

test('the description names both timings, because they differ by three orders of magnitude', async () => {
  // Showing only the total makes a translator regression invisible: 54,764ms and 54,800ms are the
  // same sentence. The two numbers are the reason this is worth a function at all.
  const env = fakeBackend()
  const job = await startExport(env.backend)
  env.finish(reply(4245))
  await job.settled()

  const text = describeJob(job)
  ok(text.includes('4245'), `should say the page count: ${text}`)
  ok(text.includes('54.8s'), `should say the total: ${text}`)
  ok(text.includes('35ms'), `should say the translating share: ${text}`)
  ok(text.includes('45s'), `should say the typesetting share: ${text}`)
  return { described: text }
})

test('a running job describes itself in words, and only shows seconds once there are any', async () => {
  const env = fakeBackend()
  const job = await startExport(env.backend)
  env.report(job.jobId, 'layout', 400, 'Typesetting pages')
  await settle()
  const early = describeJob(job)
  ok(!early.includes('('), `under a second there is nothing to say about time: ${early}`)
  ok(early.includes('Typesetting pages'), 'but the phase is there')

  env.report(job.jobId, 'layout', 45_000, 'Typesetting pages')
  await settle()
  const later = describeJob(job)
  ok(later.includes('(45s)'), `after a second the elapsed time is worth showing: ${later}`)

  env.finish()
  await job.settled()
  return { early, later }
})

test('every state has a description, and none of them is empty', async () => {
  // A phase with an empty string is a blank status line, and `describeJob` is the single place
  // the wording lives so the status bar, the panel and any notification cannot disagree.
  const env = fakeBackend()
  const job = await startExport(env.backend)
  const states: ExportJob[] = [job]
  for (const text of [describeJob(job)]) ok(text.length > 0, 'a running job has a description')

  env.abort(`${CANCELLED_PREFIX}: stopped`)
  await job.settled()
  ok(describeJob(job).toLowerCase().includes('cancel'), `expected a cancellation, got "${describeJob(job)}"`)

  const failing = await startExport(env.backend)
  states.push(failing)
  env.abort('could not typeset "X"')
  try {
    await failing.settled()
  } catch {
    // Expected.
  }
  await settle()
  const failureText = describeJob(failing)
  ok(failureText.toLowerCase().includes('failed'), `expected a failure, got "${failureText}"`)
  ok(failureText.includes('could not typeset'), 'and the reason should be carried through')
  return { states: states.length }
})

test('two jobs started together keep separate state', async () => {
  // The last-writer-wins version of this module would have one `state` variable, and the second
  // job's phase would overwrite the first's. Two jobs is the smallest case that catches it.
  const env = fakeBackend()
  const first = await startExport(env.backend)
  const second = await startExport(env.backend)

  env.report(first.jobId, 'layout', 1_000)
  await settle()
  env.report(second.jobId, 'translating', 5)
  await settle()
  env.report(first.jobId, 'serializing', 50_000)
  await settle()

  const a = first.state
  const b = second.state
  if (a.kind !== 'running' || b.kind !== 'running') throw new Error('both should be running')
  ok(a.phase === 'serializing', `the first job should be at serializing, got ${a.phase}`)
  ok(b.phase === 'translating', `the second job should be at translating, got ${b.phase}`)
  ok(a.elapsedMs === 50_000 && b.elapsedMs === 5, 'each keeps its own elapsed time')

  env.finish()
  env.finish()
  await Promise.all([first.settled(), second.settled()])
  return { first: a.phase, second: b.phase }
})

await Promise.all(pending)

console.log('='.repeat(72))
console.log(`${passed} passed, ${failed} failed`)
if (failed) {
  console.log(`failing: ${failures.join(', ')}`)
  process.exit(1)
}
