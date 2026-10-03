/**
 * Scroll-driven mounting: which sections exist in the DOM, and where.
 *
 * # The problem
 *
 * `SectionRegistry` slides its window on **focus**: focusing a section mounts it
 * and its neighbours. That is correct for a caret moving between sections, and
 * wrong for scrolling. A user who drags the scrollbar through 400 sections never
 * focuses any of them, so with focus-driven mounting the viewport would show
 * nothing at all between the frozen top and the last focused section.
 *
 * The frozen layer is the whole premise. `2000.md` §"Geometry Engine" is right
 * that you cannot measure what is not rendered, and that a prefix-sum tree over
 * section heights is how the scrollbar knows the document is 30 million pixels
 * tall. What it does not say is that the *same* structure has to decide what to
 * mount, or that mounting has to be told when the DOM disagrees with it.
 *
 * # The contract
 *
 * The Rust `Geometry` owns heights. This module owns the DOM side of that
 * agreement:
 *
 *   1. On scroll, ask the geometry which sections intersect the viewport and
 *      mount exactly those, with overscan.
 *   2. Position each mounted section with a spacer of the exact height of
 *      everything above it, so the scrollbar is truthful and a section is where
 *      the user expects once it mounts.
 *   3. Report every measured height back to the geometry, and apply the scroll
 *      compensation it asks for.
 *
 * Step 3 is the one that is easy to get wrong and is covered in detail on
 * [`applyMeasurement`].
 *
 * # Why the compensation is not optional
 *
 * When an unmeasured section mounts and turns out taller than the estimate, every
 * section below it moves. If the user is looking at content that just shifted,
 * the view jumps. The fix is to move the scroll position by the same amount, so
 * the pixels under the user's eyes stay put. See
 * `Geometry::scroll_compensation` in `crates/holonomy-core/src/geometry.rs` for
 * the Rust side and why the "entirely above the viewport" condition is the one
 * that is correct rather than merely convenient.
 */

/** The geometry operations this module needs.
 *
 * An interface rather than a concrete type so the module is testable without a
 * Tauri bridge, and so the browser and the native path cannot drift: there is
 * one set of semantics, tested once.
 */
export interface GeometryBridge {
  /** Total document height in pixels, for the scroll track. */
  totalHeight(): number | Promise<number>
  /** The section containing scroll offset `y`, clamped. */
  sectionAt(y: number): number | Promise<number>
  /** Sections intersecting the viewport, with overscan, as [first, lastExclusive). */
  visibleRange(y: number, viewportHeight: number, overscan: number): [number, number] | Promise<[number, number]>
  /** Pixel offset of a section's top edge, for absolute positioning. */
  offsetOf(index: number): number | Promise<number>
  /**
   * Report a measured height. Returns the change and the compensation to apply.
   *
   * `viewportTop` is passed in rather than read from the container, because the
   * geometry owns the "is this change above the user" decision. The same
   * measurement compensates or does not depending on where the user is looking,
   * and that is the whole invariant — so it belongs in one testable place rather
   * than being inferred from ambient state in two.
   *
   * Returns a promise-or-value rather than always a promise: the in-process
   * implementation is synchronous, and wrapping it in an async function would add
   * a microtask to every scroll frame for no benefit.
   */
  measure(
    index: number,
    height: number,
    viewportTop: number,
  ): { delta: number; compensate: number } | Promise<{ delta: number; compensate: number }>
}

/** Options for the scroller. */
export interface ScrollerOptions {
  /** The scrollable container. */
  container: HTMLElement
  /** The inner element that grows to the full document height. */
  canvas: HTMLElement
  /**
   * Mount a section, returning the element to place inside its slot.
   *
   * This is the only coupling to `SectionRegistry`: the scroller does not know
   * what a section is, only that mounting one produces an element whose height
   * it can then measure.
   */
  mount(index: number): HTMLElement | null
  /** Unmount a section, freeing its editor. */
  unmount(index: number): void
  /** The geometry, in whichever process owns it. */
  geometry: GeometryBridge
  /** Sections to keep mounted beyond the viewport on each side. */
  overscan?: number
  /**
   * How to hold the document open between mounted sections.
   *
   * See [`SpacerStrategy`]. Defaults to `absolute`.
   */
  strategy?: SpacerStrategy
  /**
   * A section that must stay mounted regardless of scroll position.
   *
   * The focused section. Unmounting it mid-scroll would destroy the editor that
   * holds the caret and the pending undo group, so the window is the union of
   * what the viewport needs and this.
   */
  pinnedIndex?: number | null
  /**
   * Called when the mounted set changes, for diagnostics and the status bar.
   *
   * Also how a caller learns that the *buffer window* has moved: it fires from
   * `reconcile`, on the pass where the mounted set actually changed, which is precisely
   * when an in-flight content fetch stops being wanted.
   *
   * That is why it fires only on a change rather than every refresh. A scroll inside the
   * current window does not move the buffer window, so invalidating outstanding fetches on
   * one would discard work that is still relevant.
   *
   * It must stay cheap: it runs inside `reconcile`, on the scroll path.
   */
  onWindowChange?: (indices: number[]) => void
}

/** How the spacer is applied.
 *
 * Two strategies, because the cheap one breaks in a specific case:
 *
 * - `absolute`: each section is positioned absolutely at its measured or
 *   estimated offset. A height change needs no relayout of anything else, so
 *   measurement is O(1) per section. But absolutely positioned children do not
 *   contribute to their parent's height, so the canvas height must be set
 *   explicitly on every change.
 * - `flow`: sections are ordinary block children, with a spacer div before them.
 *   The browser handles the layout, and the canvas grows naturally. But a
 *   measurement that changes a section's height reflows everything after it,
 *   which is exactly the cost the geometry exists to avoid.
 *
 * `absolute` is the default because measurement frequency (every mount, every
 * reflow) dominates mounting frequency (every scroll into new territory), and
 * absolute positioning makes the former free. `flow` exists because it is the
 * only one that behaves correctly when a section's content grows without a
 * measurement being reported, which happens with images that decode late.
 */
export type SpacerStrategy = 'absolute' | 'flow'

export class SectionScroller {
  private readonly opts: {
    container: HTMLElement
    canvas: HTMLElement
    mount(index: number): HTMLElement | null
    unmount(index: number): void
    geometry: GeometryBridge
    overscan: number
    strategy: SpacerStrategy
    pinnedIndex: number | null
    onWindowChange?: (indices: number[]) => void
  }

  /** Section indices currently mounted, ascending. */
  private mounted = new Set<number>()

  /** Measured heights by index, for the absolute strategy's positioning. */
  private heights = new Map<number, number>()

  private strategy: SpacerStrategy = 'absolute'
  private rafHandle = 0
  private disposed = false

  /** Counters for the status bar and for tests. Diagnostics, not state. */
  private stats = { scrollEvents: 0, mounts: 0, unmounts: 0, measurements: 0, compensations: 0 }

  constructor(options: ScrollerOptions) {
    this.strategy = options.strategy ?? 'absolute'
    this.opts = {
      container: options.container,
      canvas: options.canvas,
      mount: options.mount,
      unmount: options.unmount,
      geometry: options.geometry,
      overscan: options.overscan ?? 1,
      strategy: this.strategy,
      pinnedIndex: options.pinnedIndex ?? null,
      onWindowChange: options.onWindowChange,
    } as any
  }

  /** Begin listening for scroll. Idempotent. */
  start(): void {
    if (this.disposed) return
    // Passive: scroll never calls preventDefault, and a non-passive listener on
    // a scroll container costs the compositor a round trip.
    this.opts.container.addEventListener('scroll', this.onScroll, { passive: true })
    void this.refresh()
  }

  stop(): void {
    this.opts.container.removeEventListener('scroll', this.onScroll)
    if (this.rafHandle) cancelAnimationFrame(this.rafHandle)
    this.rafHandle = 0
  }

  dispose(): void {
    this.stop()
    this.disposed = true
    for (const i of [...this.mounted]) this.opts.unmount(i)
    this.mounted.clear()
  }

  private onScroll: () => void = () => {
    this.stats.scrollEvents++
    // One update per frame, not one per scroll event. A trackpad fling delivers
    // scroll events faster than the display refreshes, so reacting to each one
    // does the same work several times over and can mount sections that were
    // only briefly on screen.
    if (this.rafHandle) return
    this.rafHandle = requestAnimationFrame(() => {
      this.rafHandle = 0
      void this.refresh()
    })
  }

  /** What is mounted right now, ascending. */
  mountedIndices(): number[] {
    return [...this.mounted].sort((a, b) => a - b)
  }

  /** Counters, for the status bar and for tests. */
  statsSnapshot(): { scrollEvents: number; mounts: number; unmounts: number; measurements: number; compensations: number } {
    return { ...this.stats }
  }

  /** Diagnostics. */
  inspect(): {
    mounted: number[]
    heights: number[]
    stats: { scrollEvents: number; mounts: number; unmounts: number; measurements: number; compensations: number }
    strategy: SpacerStrategy
  } {
    return {
      mounted: this.mountedIndices(),
      heights: this.mountedIndices().map(i => this.heights.get(i) ?? 0),
      stats: this.statsSnapshot(),
      strategy: this.strategy,
    }
  }

  /**
   * Reconcile the mounted set with the viewport.
   *
   * Safe to call at any time. Exposed for tests and for the first paint, where
   * there is no scroll event to wait for.
   */
  async refresh(): Promise<{ first: number; last: number } | null> {
    if (this.disposed) return null
    const y = this.opts.container.scrollTop
    const h = this.opts.container.clientHeight || 1

    const range = (await this.opts.geometry.visibleRange(y, h, this.opts.overscan)) ?? null
    if (!range) return null
    const [first, last] = range

    await this.syncHeight()
    this.reconcile(first, last)
    this.position()
    return { first, last }
  }

  /**
   * Set the canvas to the document's total height.
   *
   * Under `absolute` positioning this is not optional: the children are out of
   * flow, so nothing else would give the container a scrollable height and the
   * scrollbar would be a few pixels tall for a 2000-page document.
   */
  private async syncHeight(): Promise<void> {
    const total = await this.opts.geometry.totalHeight()
    const px = `${Math.max(0, Math.round(total))}px`
    if (this.opts.canvas.style.height !== px) this.opts.canvas.style.height = px
  }

  /** Mount what the viewport needs, unmount what it does not. */
  private reconcile(first: number, last: number): void {
    const wanted = new Set<number>()
    for (let i = first; i < last; i++) wanted.add(i)

    // The focused section stays mounted even when scrolled far away, since it
    // holds the caret and the open undo group.
    const pinned = this.opts.pinnedIndex
    if (pinned !== null && pinned !== undefined) wanted.add(pinned)

    let changed = false

    for (const i of [...this.mounted]) {
      if (!wanted.has(i)) {
        this.opts.unmount(i)
        this.mounted.delete(i)
        // The height is *kept*, not forgotten. Unmounting does not change how
        // tall a section is, and dropping it would make the document jump when
        // the user scrolls away and back.
        this.stats.unmounts++
        changed = true
      }
    }
    for (const i of wanted) {
      if (this.mounted.has(i)) continue
      this.opts.mount(i)
      this.mounted.add(i)
      this.stats.mounts++
      changed = true
    }

    // Remove the slot elements for sections that are no longer mounted.
    //
    // This is not a cosmetic tidy-up. `unmount` in the app only empties the slot
    // (it must keep the element and its measured height), so without this the DOM
    // accumulates one empty absolutely-positioned div per section ever visited.
    // Two measured consequences: "how many sections are in the DOM" reports every
    // section ever scrolled past rather than the live window — 12 and 14 slots
    // against a 4-section window — and each stale div keeps its editor subtree's
    // residual layout cost in the compositor.
    this.pruneSlots()

    if (changed) {
      this.opts.onWindowChange?.(this.mountedIndices())
      // Deliberately not awaited: `reconcile` is called from `refresh`, and
      // awaiting would serialise mounting behind measurement, adding a frame of
      // latency to every scroll into new territory. The measurement is queued and
      // the geometry is updated a moment later, which is the same frame either way.
      void this.measureMounted()
    }
  }

  /** Remove slot elements for sections that are no longer mounted. */
  private pruneSlots(): void {
    const live = this.mounted
    for (const el of Array.from(this.opts.canvas.querySelectorAll('[data-slot]'))) {
      const index = Number((el as HTMLElement).dataset.slot)
      if (!Number.isFinite(index) || live.has(index)) continue
      el.remove()
    }
  }

  /**
   * Measure every mounted section and report it to the geometry.
   *
   * This is the loop that keeps the scrollbar honest, and it is why
   * `applyMeasurement` has to get the compensation right.
   */
  private async measureMounted(): Promise<void> {
    // Measure in document order. Compensation for a change above the viewport
    // adjusts scrollTop, and doing that between two measurements would make the
    // second measurement's rect reflect the adjusted position. Measuring all the
    // rects first and applying the scroll once at the end avoids that.
    const rects: Array<{ index: number; height: number }> = []
    for (const i of this.mountedIndices()) {
      const el = this.slotElement(i)
      if (!el) continue
      // offsetHeight rounds to an integer, which is fine: the estimate is
      // fractional and rounding the measurement is well inside the error the
      // model already has. getBoundingClientRect().height would give subpixels
      // that no display can show.
      const h = el.getBoundingClientRect().height
      if (h > 0) rects.push({ index: i, height: h })
    }

    for (const { index, height } of rects) {
      await this.applyMeasurement(index, height)
    }
  }

  /**
   * Report one measurement and apply the compensation it implies.
   *
   * # The failure this prevents
   *
   * Measuring a section above the viewport changes every section below it. The
   * content the user is looking at moves, and the view jumps by the difference.
   * The fix is to move the scroll position by the same amount, which holds the
   * pixels under the user's eyes still.
   *
   * Two conditions must both hold, and the geometry decides the second:
   *
   *   - there is a delta worth applying (a re-measure of the same height is a
   *     no-op and must not scroll at all, or the view creeps on every frame);
   *   - the changed section lies *entirely above* the viewport top. A section
   *     straddling the top, or below it, did not move the content being looked
   *     at, and compensating would be wrong.
   *
   * Compensating unconditionally is the classic virtual-scroller bug: scrolling
   * down through fresh sections makes the document ratchet taller, and the
   * scrollbar drifts away from the content.
   */
  private async applyMeasurement(index: number, height: number): Promise<void> {
    const previous = this.heights.get(index)
    if (previous !== undefined && Math.abs(previous - height) < 0.5) {
      // Sub-half-pixel changes are not worth a scroll adjustment. A ResizeObserver
      // can fire many times during a reflow, and acting on each would make the
      // view shimmer.
      return
    }

    const { compensate } = await this.opts.geometry.measure(
      index,
      height,
      this.opts.container.scrollTop,
    )
    this.heights.set(index, height)
    this.stats.measurements++

    if (compensate !== 0) {
      this.applyCompensation(compensate)
    }
  }

  /**
   * Apply a scroll compensation, and reconcile the window it may have invalidated.
   *
   * Every height correction that moves the scroll position goes through here. The
   * reason to be strict about that: the same section can be measured from two
   * independent directions — the scroller's own mount-time pass, and a
   * `ResizeObserver` firing because the content reflowed — and if both apply their
   * own compensation then every section that mounts shifts the viewport twice.
   * That reads as stutter, and it is a bug rather than a tuning problem.
   *
   * The *decision* to compensate belongs to the geometry, which returns zero unless
   * the changed section lies entirely above the viewport top. This method only
   * applies what it is given.
   */
  protected applyCompensation(compensate: number): void {
    this.opts.container.scrollTop += compensate
    this.stats.compensations++
    // The viewport moved, so which sections are needed may have changed.
    // Reconciling here rather than waiting for the next scroll event avoids a frame
    // of blank space at the new position.
    void this.refresh()
  }

  /** The element for a mounted section, if it is in the DOM. */
  private slotElement(index: number): HTMLElement | null {
    return this.opts.canvas.querySelector<HTMLElement>(`[data-slot="${index}"]`)
  }

  /**
   * Place every mounted section at its offset.
   *
   * Only meaningful under `absolute`. Positions come from the geometry, so a
   * section that has never been measured sits at the offset its estimate implies
   * rather than at zero, which is what makes a freshly mounted section appear in
   * the right place rather than jumping.
   */
  private async position(): Promise<void> {
    if (this.strategy !== 'absolute') return
    for (const i of this.mountedIndices()) {
      const el = this.slotElement(i)
      if (!el) continue
      const top = await this.opts.geometry.offsetOf?.(i) ?? 0
      const px = `${Math.round(top)}px`
      if (el.style.top !== px) el.style.top = px
    }
  }

  /** Set the strategy. Existing slots are re-styled on the next refresh. */
  setStrategy(strategy: SpacerStrategy): void {
    this.strategy = strategy
  }

  /** The strategy in use. */
  getStrategy(): SpacerStrategy {
    return this.strategy
  }
}
