/**
 * The height sync batches; it does not debounce.
 *
 * # The distinction, because it is the whole design
 *
 * A **debounce** waits for quiescence — each measurement resets the timer, and the
 * flush happens after the last one. Under continuous layout shift (a paragraph
 * reflowing as it is typed, a window drag, images decoding) measurements never stop,
 * so a debounced flush *never fires*. The queue grows and Rust's tree drifts
 * arbitrarily far from the truth.
 *
 * A **throttle** fires at most once per interval, at the end of it. Measurements
 * arriving during a window join the batch that window will send. So continuous
 * shifting produces one call per interval rather than none.
 *
 * `continuous layout shift becomes exactly one IPC call` is the directive, and it is
 * the assertion that separates the two. A debounce implementation passes a test that
 * counts calls after a *quiet* period and fails this one, which is the point of
 * writing it this way.
 *
 * # Why Node and not a browser
 *
 * Nothing here touches layout. `HeightSync` takes its transport as a constructor
 * argument precisely so it can be tested without a Tauri host or a rendering engine,
 * and a test that needed a browser to count timer callbacks would be testing the
 * browser.
 *
 * Run: node --experimental-strip-types test/height-sync.ts
 */

import { HeightSync, type HeightTransport } from '../src/core/height-sync.ts'
import type { HeightUpdate } from '../src/core/boot.ts'

let passed = 0
let failed = 0
const failures: string[] = []

function test(name: string, fn: () => Promise<unknown> | unknown): Promise<void> {
  return Promise.resolve()
    .then(fn)
    .then(detail => {
      passed++
      console.log(`PASS  ${name}${detail !== undefined ? `  ${JSON.stringify(detail)}` : ''}`)
    })
    .catch((e: any) => {
      failed++
      failures.push(name)
      console.log(`FAIL  ${name}\n        ${e.message}`)
    })
}

function ok(cond: unknown, msg: string): asserts cond {
  if (!cond) throw new Error(msg)
}

/** A transport that records what it was asked to send. */
function recorder() {
  const batches: HeightUpdate[][] = []
  let failNext = 0
  const send: HeightTransport = async updates => {
    batches.push(updates.map(u => ({ ...u })))
    if (failNext > 0) {
      failNext--
      throw new Error('transport refused')
    }
    return { total_height: 0, delta: 0, sections: updates.length }
  }
  return {
    send,
    batches,
    /**
     * A method rather than a getter, and not for style.
     *
     * `ok(rec.calls() === 0, ...)` is an assertion, so TypeScript narrows a getter to
     * the literal `0` afterwards and then rejects `rec.calls() === 1` as comparing two
     * types with no overlap. The narrowing is correct and useless: the count really
     * does change, between the assertion and the next line. A method call is not
     * narrowed, so the counter stays a number.
     */
    calls: () => batches.length,
    failOnce() {
      failNext = 1
    },
  }
}

/** A clock the test drives, so window boundaries are exact rather than timing-dependent. */
function fakeClock() {
  let t = 0
  return {
    now: () => t,
    advance: (ms: number) => {
      t += ms
    },
  }
}

const sleep = (ms: number) => new Promise(r => setTimeout(r, ms))

async function main() {
  console.log('height sync batches; it does not debounce')
  console.log('='.repeat(56))

  // -----------------------------------------------------------------------

  await test('continuous layout shift becomes exactly one IPC call', async () => {
    // The directive's test, and the one a debounce fails.
    //
    // 200 measurements over 200ms — faster than the 300ms window, so every one of
    // them lands inside the same window. A trailing debounce would fire once at the
    // end and this would also pass, so the second assertion below is what actually
    // pins it: the call must arrive *while the shifting continues*, not after it
    // stops.
    const rec = recorder()
    const clock = fakeClock()
    const sync = new HeightSync({ send: rec.send, now: clock.now })

    // The section is being measured over and over as one paragraph reflows.
    for (let i = 0; i < 200; i++) {
      sync.record(0, 's0', 300 + i * 0.5)
      clock.advance(1)
    }
    ok(rec.calls() === 0, `nothing should have been sent mid-window, got ${rec.calls()}`)

    // Shift still in progress when the window closes.
    clock.advance(300)
    await sync.flush()
    ok(
      rec.calls() === 1,
      `continuous shifting must produce one call per window, got ${rec.calls()}`,
    )

    // And it kept going: a second window, still shifting, one more call. A debounce
    // would have produced zero for both.
    for (let i = 0; i < 200; i++) {
      sync.record(0, 's0', 400 + i * 0.5)
      clock.advance(1)
    }
    clock.advance(300)
    await sync.flush()
    ok(rec.calls() === 2, `the second window must also send; got ${rec.calls()} calls`)

    await sync.dispose()
    return { calls: rec.calls() }
  })

  await test('the timer fires without an explicit flush', async () => {
    // The previous test drives `flush` by hand. This one uses only the real timer, so
    // a test cannot pass while the scheduling is broken — `flush` is public, and a
    // harness that only ever calls it would never notice that `setTimeout` was never
    // reached.
    const rec = recorder()
    const sync = new HeightSync({ send: rec.send, intervalMs: 40 })
    sync.record(0, 's0', 500)
    sync.record(1, 's1', 600)

    await sleep(120)
    ok(rec.calls() === 1, `the timer should have fired once, got ${rec.calls()}`)
    ok(
      JSON.stringify(rec.batches[0]!.map(u => u.section_id)) === JSON.stringify(['s0', 's1']),
      `a batch should carry every section measured in the window, got ${JSON.stringify(rec.batches[0])}`,
    )
    await sync.dispose()
  })

  await test('a section measured repeatedly appears once, at its final height', async () => {
    // Absolute heights, not deltas. Forty measurements of one section must send one
    // entry with the last value, not forty entries and not a running sum — which is
    // what makes the queue idempotent and safe to retry.
    const rec = recorder()
    const clock = fakeClock()
    const sync = new HeightSync({ send: rec.send, now: clock.now })

    for (let i = 0; i < 40; i++) {
      sync.record(3, 's3', 1000 + i)
      clock.advance(2)
    }
    clock.advance(300)
    await sync.flush()

    const batch = rec.batches[0]!
    ok(batch.length === 1, `expected one entry, got ${batch.length}`)
    ok(batch[0]!.height === 1039, `expected the final height 1039, got ${batch[0]!.height}`)
    await sync.dispose()
    return { batch: rec.batches[0] }
  })

  await test('a batch is ordered by index regardless of record order', async () => {
    // The geometry is index-keyed, so a batch that arrived out of order would still
    // be correct — but a log or a test reading it would be confusing, and ordering by
    // index makes "what did we send" answerable by reading it.
    const rec = recorder()
    const clock = fakeClock()
    const sync = new HeightSync({ send: rec.send, now: clock.now })

    for (const i of [7, 2, 5, 0, 3]) sync.record(i, `s${i}`, 100 * i)
    clock.advance(300)
    await sync.flush()

    ok(
      JSON.stringify(rec.batches[0]!.map(u => u.index)) === JSON.stringify([0, 2, 3, 5, 7]),
      `expected index order, got ${JSON.stringify(rec.batches[0]!.map(u => u.index))}`,
    )
    await sync.dispose()
  })

  await test('a quiet document sends nothing at all', async () => {
    // The other half of batching: no layout change, no IPC. A sync that fires on a
    // timer regardless of whether anything changed would be pure overhead.
    const rec = recorder()
    const clock = fakeClock()
    const sync = new HeightSync({ send: rec.send, now: clock.now })

    clock.advance(5000)
    await sync.flush()
    ok(rec.calls() === 0, `nothing changed, so nothing should be sent; got ${rec.calls()}`)
    await sync.dispose()
  })

  await test('an unchanged height is not queued, across windows', async () => {
    // Re-measuring a section to the height it already has is the most common event in
    // the system: the ResizeObserver fires for the whole mounted window on any layout
    // change, and the scroller re-measures mounted sections on every refresh. So most
    // measurements during a scroll are of sections whose height did not move.
    //
    // Queuing those would put a round trip on the bridge carrying a value Rust already
    // has. Harmless — `update_measured_height` treats an identical height as a no-op —
    // but paid on the most frequent event, for no information.
    //
    // The "across windows" part is the point. The queue is drained every window, so a
    // gate that only compared against the queue would pass a within-window test and
    // fail this one, which is the same shape of mistake as the debounce/throttle
    // distinction.
    const rec = recorder()
    const clock = fakeClock()
    const sync = new HeightSync({ send: rec.send, now: clock.now })

    ok(sync.record(0, 's0', 500), 'a first measurement is a change')
    clock.advance(300)
    await sync.flush()
    ok(rec.calls() === 1, 'precondition: the first measurement is sent')

    ok(!sync.record(0, 's0', 500), 'an identical height is not a change')
    clock.advance(300)
    await sync.flush()
    ok(rec.calls() === 1, `an identical height must not be re-sent; got ${rec.calls()} calls`)

    // And a real change after the no-op is still noticed.
    ok(sync.record(0, 's0', 520), 'a different height is a change')
    clock.advance(300)
    await sync.flush()
    ok(rec.calls() === 2, `a real change must be sent; got ${rec.calls()} calls`)
    ok(rec.batches[1]![0]!.height === 520, `expected 520, got ${rec.batches[1]![0]!.height}`)
    await sync.dispose()
  })

  await test('a measurement of a section that has not changed opens no window', async () => {
    // The other half: a no-op must not even schedule a flush. A window that opens on
    // every observation would fire a timer per frame during a scroll, and each one
    // would find an empty queue.
    const rec = recorder()
    const clock = fakeClock()
    const sync = new HeightSync({ send: rec.send, now: clock.now })

    sync.record(0, 's0', 500)
    clock.advance(300)
    await sync.flush()
    ok(rec.calls() === 1, 'precondition: one send')

    for (let i = 0; i < 100; i++) {
      sync.record(0, 's0', 500)
      clock.advance(1)
    }
    clock.advance(300)
    await sync.flush()
    ok(rec.calls() === 1, `100 identical measurements must produce no traffic; got ${rec.calls()}`)
    await sync.dispose()
  })

  await test('a measurement that is not a layout is refused', async () => {
    // Zero and NaN are what a detached or unlaid-out element reports. `LocalGeometry.
    // measure` rejects them; if this did not, a section's height would be sent as
    // nothing and Rust would collapse it to a zero-height row.
    const rec = recorder()
    const sync = new HeightSync({ send: rec.send })
    ok(!sync.record(0, 's0', Number.NaN), 'NaN must be refused')
    ok(!sync.record(0, 's0', -5), 'a negative height must be refused')
    ok(!sync.record(-1, 's0', 100), 'a negative index must be refused')
    ok(sync.pendingCount() === 0, `nothing should be queued, pending=${sync.pendingCount()}`)
    await sync.dispose()
  })

  await test('a failed flush is re-queued, not lost', async () => {
    // The queue holds absolute heights, so a retry is safe: the same values again, and
    // `update_measured_height` treats an identical height as a no-op. Losing them
    // would leave Rust's total quietly wrong, which is the one failure mode with no
    // symptom at all.
    const rec = recorder()
    const clock = fakeClock()
    rec.failOnce()
    const sync = new HeightSync({ send: rec.send, now: clock.now })

    sync.record(0, 's0', 700)
    clock.advance(300)
    await sync.flush()
    ok(sync.pendingCount() === 1, `a failed batch must stay queued, pending=${sync.pendingCount()}`)
    ok(rec.calls() === 1, 'one attempt was made')

    clock.advance(300)
    await sync.flush()
    ok(rec.calls() === 2, 'the next window must retry it')
    ok(
      rec.batches[1]![0]!.height === 700,
      `the retry must carry the same absolute height, got ${rec.batches[1]![0]!.height}`,
    )
    ok(sync.pendingCount() === 0, `the queue should be empty after a successful retry, got ${sync.pendingCount()}`)
    await sync.dispose()
  })

  await test('a measurement taken during a send goes in the next batch, not this one', async () => {
    // The interesting interleaving: the send is in flight when a new measurement
    // arrives. Sending it here would mean a section appears in two batches with the
    // older value sent last, and Rust would end up with the wrong height.
    const batches: HeightUpdate[][] = []
    let release: (() => void) | null = null
    const send: HeightTransport = async updates => {
      batches.push(updates.map(u => ({ ...u })))
      if (batches.length === 1) {
        // Hold the first send open until a second measurement has been recorded. The
        // release function is awaited by the test, which is what makes the
        // interleaving deterministic rather than timing-dependent.
        await new Promise<void>(r => {
          release = r
        })
      }
      return null
    }
    const sync = new HeightSync({ send, intervalMs: 300 })

    sync.record(0, 's0', 100)
    const inflight = sync.flush()
    await sleep(5)

    // Lands while the first send is open.
    sync.record(0, 's0', 200)
    ok(sync.pendingCount() === 1, `the new measurement should be queued, pending=${sync.pendingCount()}`)

    release!()
    await inflight
    const outByThen = batches.length
    ok(outByThen === 1, `only one batch should have gone out, got ${outByThen}`)
    ok(batches[0]![0]!.height === 100, `the in-flight batch must carry 100, got ${batches[0]![0]!.height}`)

    await sync.flush()
    // Read through a local rather than `batches.length`: the assertion above narrows
    // it to the literal `1`, and the second send really does make it `2`.
    const sent = batches.length
    ok(sent === 2, `the later measurement should be sent next, got ${sent} calls`)
    ok(batches[1]![0]!.height === 200, `the second batch must carry 200, got ${batches[1]![0]!.height}`)
    await sync.dispose()
  })

  await test('dispose sends what is queued', async () => {
    // The last measurement before a window closes is the document's final shape, and
    // it is the one most worth having. A teardown that dropped it would leave Rust
    // describing a document one edit old.
    const rec = recorder()
    const sync = new HeightSync({ send: rec.send, intervalMs: 10_000 })
    sync.record(4, 's4', 900)
    await sync.dispose()
    ok(rec.calls() === 1, `dispose must flush, got ${rec.calls()} calls`)
    ok(rec.batches[0]![0]!.section_id === 's4', 'the queued section should have been sent')
  })

  await test('stats distinguish sends from sections sent', async () => {
    // Two sections in one window is one send and two sections. A metric that counted
    // both as one would understate the traffic on the bridge; one that counted them
    // as two would overstate the round trips. The cross-engine harness reports these,
    // so they have to mean what they say.
    const rec = recorder()
    const clock = fakeClock()
    const sync = new HeightSync({ send: rec.send, now: clock.now })

    sync.record(0, 's0', 100)
    sync.record(1, 's1', 200)
    clock.advance(300)
    await sync.flush()
    sync.record(2, 's2', 300)
    clock.advance(300)
    await sync.flush()

    const stats = sync.snapshot()
    ok(stats.sends === 2, `expected 2 sends, got ${stats.sends}`)
    ok(stats.sent === 3, `expected 3 sections sent, got ${stats.sent}`)
    ok(stats.failures === 0, 'no failures expected')
    ok(
      JSON.stringify(stats.lastBatch) === JSON.stringify(['s2']),
      `lastBatch should describe the last window, got ${JSON.stringify(stats.lastBatch)}`,
    )
    await sync.dispose()
    return stats
  })

  console.log('='.repeat(56))
  console.log(`${passed} passed, ${failed} failed`)
  if (failed) {
    console.log(`failing: ${failures.join(', ')}`)
    process.exit(1)
  }
}

await main()
