/**
 * The bridge: the only place the frontend talks to Rust.
 *
 * # Three commands, and why there are only three
 *
 * - `get_document_boot` — once, at startup. Everything needed for a first frame.
 * - `sync_section_heights` — debounced batches of measured heights.
 * - `commit_section_lifecycle` — splits and merges, rare.
 *
 * Nothing else crosses. In particular there is no command for "where is section 37",
 * because the manifest the boot payload carries already answers that, and a scroll
 * that needed a round trip to learn a height would not be a scroll.
 *
 * # MessagePack for exactly one of them
 *
 * The boot payload is the only message on the path to first paint, and for a
 * 2000-page document it is ~50KB of manifest plus ~84KB of compressed content.
 * JSON would mean parsing that as text before anything renders. Every other message
 * is JSON, because those are small and being able to read one in a log is worth more
 * than the bytes.
 *
 * # No bridge, no crash
 *
 * In a plain browser — the test harness, `npm run dev` — there is no Tauri, and this
 * module reports that rather than throwing. The harness builds its own synthetic
 * document, which is why it has never needed a store, and forcing one would mean the
 * tests measured a path the product does not use.
 */

import { decode as msgpackDecode, encode as msgpackEncode } from '@msgpack/msgpack'
import type {
  BackupReply,
  BootPayload,
  CommitResponse,
  EphemeralDocument,
  ExportPhase,
  ExportProgress,
  ExportReply,
  HeightUpdate,
  LifecycleAction,
  LifecycleResult,
  OptimizeReport,
  SearchResponse,
  SectionContent,
} from './boot'

export {
  type BackupReply,
  type BootPayload,
  type OptimizeReport,
  type CommitResponse,
  type EphemeralDocument,
  type ExportPhase,
  type ExportProgress,
  type ExportReply,
  type HeightUpdate,
  type LifecycleAction,
  type LifecycleResult,
  type SectionContent,
}

/** True when a Tauri host is present, so the real commands can be called. */
export function hasBridge(): boolean {
  return typeof (globalThis as any).__TAURI__?.core?.invoke === 'function'
}

/**
 * Invoke a bridge command.
 *
 * The engine identity comes from Rust rather than from user-agent sniffing
 * (webkit2gtk's UA is unreliable, and two Chromium versions on two platforms would
 * look the same anyway), and this is the same door it comes through.
 */
/**
 * The event an export emits as it goes. Mirrors `EXPORT_PROGRESS_EVENT` in `lib.rs`.
 *
 * Defined here rather than in `core/export.ts` because this file is the transport: it is the one
 * that knows how the app talks to Rust, so it is the one that knows the event's name. The domain
 * module takes an `onProgress` callback and never names an event.
 */
const EXPORT_PROGRESS_EVENT = 'holo://export-progress'

/**
 * Subscribe to a Tauri event, using the global build.
 *
 * # Why this is hand-rolled
 *
 * Because `@tauri-apps/api` is not a dependency, and adding it to reach one function would pull a
 * package this project otherwise does not need. The `withGlobalTauri` build is what `invoke` above
 * already uses, so the two are consistent: one transport, one mechanism.
 *
 * The failure is loud. A subscription that silently never fires would leave a 54-second export with
 * no progress at all and no indication why, which is the exact failure this subscription exists to
 * prevent — so a missing host throws rather than returning a no-op unlisten.
 */
async function listen<T>(event: string, handler: (payload: T) => void): Promise<() => void> {
  const tauri = (globalThis as any).__TAURI__
  const raw = tauri?.event?.listen
  if (typeof raw !== 'function') {
    throw new Error(
      `cannot listen for \`${event}\`: no Tauri event host. This is a browser, not the app; the \
       test harness must supply its own progress source.`,
    )
  }
  return (await raw(event, (e: { payload: T }) => handler(e.payload))) as () => void
}

export async function invoke<T>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
  const tauri = (globalThis as any).__TAURI__
  if (!tauri?.core?.invoke) {
    throw new Error(
      `cannot invoke \`${cmd}\`: no Tauri host. This is a browser, not the app; the test ` +
        'harness builds its own document and must not reach for the bridge.',
    )
  }
  return (await tauri.core.invoke(cmd, args)) as T
}

/**
 * Coerce whatever Tauri delivered into bytes for the MessagePack decoder.
 *
 * # Why this is not one line
 *
 * `tauri::ipc::Response::new(bytes)` sends the response body with no serialisation,
 * which is the point of using it. What arrives in the renderer depends on the engine
 * and on how large the payload is: `ArrayBuffer` from webkit2gtk and WKWebView,
 * `Uint8Array` from WebView2, and a plain `number[]` if a response is ever re-encoded
 * as JSON somewhere in the path.
 *
 * All three are handled rather than one, because picking one and asserting the others
 * cannot happen would be an assertion about an implementation detail of an engine I
 * cannot test on this machine — and WKWebView and WebView2 runs are still owed. A
 * decode that throws on `number[]` fails at boot on a platform nobody tested, which
 * is the worst possible place to find out.
 */
function asBytes(raw: unknown): Uint8Array {
  if (raw instanceof Uint8Array) return raw
  if (raw instanceof ArrayBuffer) return new Uint8Array(raw)
  if (Array.isArray(raw)) return Uint8Array.from(raw as number[])
  throw new Error(
    'the boot payload arrived as ' +
      (raw === null ? 'null' : typeof raw) +
      ', which is not bytes; the MessagePack decoder needs an ArrayBuffer, a ' +
      'Uint8Array or a number array',
  )
}

/**
 * Read the document's first frame.
 *
 * # `block_count` is why this cannot be trimmed
 *
 * The manifest carries a block count per section and the geometry needs it. An
 * earlier design derived block count from characters when it was absent, and that
 * estimate measured at 225% error on short multi-paragraph sections — the stored
 * count was computed, written to SQLite, and then never used, because nothing on the
 * frontend carried it. `metrics.blocks` is required for the same reason.
 */
export async function getDocumentBoot(documentId?: string): Promise<BootPayload> {
  const raw = await invoke<unknown>('get_document_boot', { documentId: documentId ?? null })
  return msgpackDecode(asBytes(raw)) as BootPayload
}

/** What `sync_section_heights` reports back. */
export interface HeightSyncReply {
  total_height: number
  /**
   * Change in total height. Zero when every section re-measured to the height it
   * already had, which is the common case — a font load, an image decode, a window
   * resize. Skipping the compensation arithmetic when this is zero is the point of
   * returning it.
   */
  delta: number
  sections: number
}

/**
 * Send a batch of measured heights, and get back the new document height.
 *
 * # The compensation decision is not made here
 *
 * `delta` comes back so the caller can decide, but the decision belongs to whoever
 * knows the viewport top. Rust does not: `scroll_compensation` takes the viewport
 * position and this module has no idea what it is. So the batch goes up, the total
 * comes back, and `scroller.ts` applies its own rule — the same rule
 * `local-geometry.ts` mirrors.
 */
export async function syncSectionHeights(updates: HeightUpdate[]): Promise<HeightSyncReply> {
  return invoke<HeightSyncReply>('sync_section_heights', { updates })
}

/**
 * Ask for a split or a merge.
 *
 * The reply reports what actually happened rather than assuming the request
 * succeeded. A section with one block cannot be split, and that comes back as
 * `applied: false` with a reason — not as a thrown error, because the frontend asked
 * for something the document's shape does not allow and that is not a failure.
 *
 * `section_ids` is the whole new ordering, not a diff: a split moves keys on both
 * sides of the cut, so a patch would have to be exactly right about what did not
 * change.
 */
export async function commitSectionLifecycle(action: LifecycleAction): Promise<LifecycleResult> {
  return invoke<LifecycleResult>('commit_section_lifecycle', { action })
}

/**
 * Which engine this window is running on.
 *
 * From Rust, always. webkit2gtk's user-agent string does not reliably identify
 * webkit2gtk, and the cross-engine verification has to attribute every measurement
 * to a renderer — a measurement without that attribution is not comparable.
 *
 * # One function, not three call sites
 *
 * This was inline at three places: the scroller's mount report, the verification
 * runner, and this module. All three derived the engine from Rust and the *version*
 * from the same two user-agent regexes, so they could disagree about a version while
 * agreeing about the engine — and a cross-engine sweep that mis-reports a version is
 * worse than one that reports none, because it looks attributable.
 *
 * Returns `null` in a plain browser. That is a real answer, not a failure: the
 * harness genuinely is not running on any of the three shipping engines, and the
 * Chromium number it produces is labelled as the harness wherever it is recorded.
 */
export async function identifyEngine(): Promise<{ engine: string | null; version: string | null }> {
  if (!hasBridge()) return { engine: null, version: null }
  const engine = await invoke<string>('which_engine')
  return { engine, version: engineVersion() }
}

/**
 * The engine's version, from the user agent.
 *
 * # Why this is a guess, and labelled as one
 *
 * webkit2gtk and WKWebView both report a `Version/x.y` token that is the *WebKit*
 * version rather than the browser's, and WebView2 reports an Edge version. There is
 * no authoritative version string across all three, and the shell has none to offer:
 * `report_mounted` takes the frontend's guess and records it.
 *
 * So it is a guess, it is derived in exactly one place, and every field it fills is
 * optional where it is consumed. The engine *identity* — the part that decides whether
 * two runs are comparable — comes from Rust and is not a guess.
 */
function engineVersion(): string | null {
  const ua = typeof navigator === 'undefined' ? '' : navigator.userAgent
  return (
    ua.match(/Version\/([\d.]+)/)?.[1] ??
    ua.match(/Edg\/([\d.]+)/)?.[1] ??
    ua.match(/Chrome\/([\d.]+)/)?.[1] ??
    null
  )
}

/**
 * Tell the shell the frontend mounted, and which engine it mounted into.
 *
 * Until this is called nothing in the shell can tell a slow boot from a failed one,
 * and the cross-engine harness fails on a blank window rather than reporting a pass.
 *
 * A failure here is swallowed. The editor is usable without it; what is lost is the
 * engine attribution for the run, and the harness is built to fail on a missing
 * attribution rather than to guess one.
 */
export async function reportMounted(): Promise<void> {
  try {
    await invoke('report_mounted', { version: engineVersion() })
  } catch {
    // See above: a missing attribution is the harness's problem to detect, not the
    // editor's problem to crash over.
  }
}

/**
 * Hand the verification report to the shell, which writes it and exits.
 *
 * The shell is the authority on whether a run counts: it re-derives the verdict
 * rather than trusting the page's own, because a partially initialised page, a bug in
 * the runner, or a field that failed to serialise could all make a failed run look
 * passed. The exit code it sets is what `scripts/verify-engine.sh` reads.
 *
 * Failures are the caller's to handle — this is reported, not logged, because losing
 * the report means the run cannot count at all.
 */
export async function submitVerification(report: unknown): Promise<void> {
  await invoke('submit_verification', { report })
}

/**
 * Encode a boot payload, for tests that need to hand the bridge something real.
 *
 * # Why this exists rather than a literal in the test
 *
 * The three-response-shape test needs a payload genuinely encoded by the library the
 * Rust side also uses. A hardcoded byte array would drift the moment `rmp-serde` or
 * `@msgpack/msgpack` changed their output, and would then fail for a reason that has
 * nothing to do with the response shape it is testing.
 *
 * Exported rather than kept private because a test helper that cannot be imported is a
 * test helper that gets duplicated. It is not part of the bridge's surface: nothing in
 * the app calls it, and this module still contains the only encoder, which is what
 * keeps the Rust and TypeScript halves honest about being one codec.
 */
export function encodeBootPayloadForTest(payload: BootPayload): Uint8Array {
  return msgpackEncode(payload)
}

/**
 * Fetch one section's content on demand.
 *
 * MessagePack and a raw response, exactly as the boot payload. The format has to match: a
 * section fetched later must be indistinguishable from one fetched at boot, or the
 * frontend needs two decode paths and only one of them is exercised until a user scrolls
 * past the boot window.
 */
export async function getSection(sectionId: string, documentId?: string): Promise<SectionContent> {
  const raw = await invoke<unknown>('get_section', { sectionId, documentId: documentId ?? null })
  return msgpackDecode(asBytes(raw)) as SectionContent
}

/**
 * Append one edit to the write-ahead log.
 *
 * # Only `markCount` is sent
 *
 * The Rust side derives the word, character and block counts from the JSON it is about to
 * store, and returns all four. A caller-supplied `block_count` that disagreed would leave a
 * section whose stored height estimate describes different bytes than it contains, which is
 * the defect `save_section`'s own comment says it prevents. See the command's doc comment.
 *
 * `markCount` is the exception: `analyze` has no mark counter, and adding one to Rust would
 * be a second definition diverging from the frontend's `localMetrics`.
 */
export async function commitSectionEdit(
  sectionId: string,
  json: unknown,
  markCount: number,
): Promise<CommitResponse> {
  return invoke<CommitResponse>('commit_section_edit', { sectionId, json, markCount })
}

/**
 * Fold the write-ahead log, so a saved edit is in its section row.
 *
 * Separate from `commitSectionEdit` because they are different guarantees. A commit is
 * durable in the recovery buffer; this is what makes it durable in the document. The
 * eviction path needs the second one and the typing path does not.
 */
export async function flushDocument(documentId?: string): Promise<number> {
  return invoke<number>('flush_document', { documentId: documentId ?? null })
}

/**
 * Build a throwaway store-backed document, for the in-engine verification.
 *
 * # Why the harness needs a command rather than a fixture
 *
 * The LRU bound only bites when a section's bytes can be fetched again, because that is
 * the condition under which the cache is allowed to drop them. Content the frontend
 * supplied has nothing to fetch from, so the cache correctly keeps it and the bound
 * never engages. A fixture on the frontend would have measured the fixture.
 *
 * # Why it is deleted again
 *
 * It is written to the user's real database. Fifty sections of fixture prose left behind
 * by every cross-engine run would be litter beside real work, so the harness pairs this
 * with {@link deleteEphemeralDocument}.
 */
export async function createEphemeralDocument(
  sections: number,
  paragraphs = 8,
): Promise<EphemeralDocument> {
  return invoke<EphemeralDocument>('create_ephemeral_document', { sections, paragraphs })
}

/**
 * Build a throwaway document at soak scale: ~1,000,000 words across ~1300 sections.
 *
 * # Why a second command and not a bigger {@link createEphemeralDocument}
 *
 * Because that one's 512-section cap is a safety valve against a harness filling the
 * user's disk, and a soak legitimately exceeds it. Raising the cap for one caller would
 * leave the valve open for all of them, so the soak has its own command, its own cap and
 * its own title — the same `delete_ephemeral_document` cleans both up.
 */
export async function createSoakDocument(
  sections: number,
  paragraphs: number,
): Promise<EphemeralDocument> {
  return invoke<EphemeralDocument>('create_soak_document', { sections, paragraphs })
}

/**
 * Delete a fixture document, and everything hanging off it.
 *
 * Best-effort from the caller's perspective: a verification that has already got its
 * result must not fail because cleanup could not run, and the next run's document is a
 * different id anyway. So a failure is logged, not thrown.
 */
export async function deleteEphemeralDocument(documentId: string): Promise<boolean> {
  return invoke<boolean>('delete_ephemeral_document', { documentId })
}

/**
 * Store raw bytes and return the URL an image node should carry.
 *
 * # Why the frontend never hashes the bytes itself
 *
 * Because the hash *is* the address, and two implementations of SHA-256 that agree today
 * would be two that disagree after a change to either. Rust owns the digest; the frontend
 * only learns the result. `assets.ts` documents why SHA-256 rather than blake3: the frontend
 * has to be able to *verify* what the protocol handler returned, and `crypto.subtle.digest`
 * is the only digest the web platform offers.
 *
 * The bytes cross as a plain array through `invoke`, which is a JSON-ish path. That is fine
 * for the sizes this is for — a figure is tens to hundreds of KB — and it is the price of
 * not reading the image into a JavaScript array, which would be a second full copy.
 */
export async function putAsset(bytes: Uint8Array, mime: string): Promise<string> {
  return invoke<string>('put_asset', { bytes: Array.from(bytes), mime })
}

/**
 * Fetch an asset's bytes and mime over the IPC transport.
 *
 * The fallback for engines that do not dispatch `holo-asset://` requests to a Tauri custom
 * protocol handler — webkit2gtk 2.60 among them, measured and recorded in `STATUS.md`. The
 * `assets.ts` comment on `loadAssetImage` explains why the fallback goes through `invoke`
 * rather than widening the CSP.
 */
export async function getAssetBytes(
  hash: string,
): Promise<{ mime: string; bytes: number[] }> {
  const [mime, bytes] = await invoke<[string, number[]]>('get_asset', { hash })
  return { mime, bytes }
}

/**
 * Listen for an export's progress reports.
 *
 * # Why this is a wrapper rather than the app calling `listen` itself
 *
 * Because `startExport`'s contract is that the subscription is opened before the command and
 * closed when the job ends, and it cannot do either without knowing how to subscribe. Putting the
 * `listen` call here keeps that pairing in one place — and this is also the single place that
 * knows the event *name*, so a rename is one edit rather than a hunt.
 *
 * Tauri's `listen` resolves to an unlisten function, which is returned unchanged. It is not
 * called on unsubscribe because Tauri's unlisten is not idempotent-guaranteed and calling it twice
 * throws; `startExport`'s `finally` calls this exactly once per subscription either way.
 */
export async function onExportProgress(
  handler: (progress: ExportProgress) => void,
): Promise<() => void> {
  // `listen` hands the wrapper Tauri delivers, so the payload is unwrapped here rather than
  // in `core/export.ts` -- that module should never see a transport shape.
  return listen<{ payload: ExportProgress }>(EXPORT_PROGRESS_EVENT, event => handler(event.payload))
}

/**
 * Stop a running export.
 *
 * Returns whether a job was actually running. A user clicking Cancel on an export that finished a
 * moment ago is not an error, and saying so would be wrong — so the answer is reported rather than
 * turned into a failure.
 */
export async function cancelExport(jobId: string): Promise<boolean> {
  return invoke<boolean>('cancel_export', { jobId })
}

/**
 * Write a consistent snapshot of the database to one file.
 *
 * # Why the caller chooses the path
 *
 * Because where a backup goes is a user decision — a `Save As` dialog, a sync folder, a USB
 * stick. The command refuses to overwrite an existing file rather than rotating, because a backup
 * that quietly replaces the last good copy is a backup with no history.
 */
export async function backupDatabase(path: string): Promise<BackupReply> {
  return invoke<BackupReply>('backup_database', { path })
}

/**
 * Ask the platform where a backup should go, through the native save dialog.
 *
 * # Why the dialog is in Rust and not here
 *
 * Because a native file picker is a `tauri-plugin-dialog` call, and this frontend has no
 * dependency on `@tauri-apps/api` — every command goes through `invoke`. Doing the picker here
 * would mean adding the plugin package to the frontend for one call, and would split the
 * permission model across two languages: the dialog plugin is in the shell's capability file,
 * so a frontend-side dialog is the one request this application has not configured.
 *
 * `null` means the user cancelled. That is not an error and must not be reported as one — a
 * cancelled save dialog and a failed backup look identical to a user and mean opposite things.
 */
export async function pickBackupDestination(): Promise<string | null> {
  return invoke<string | null>('pick_backup_destination')
}

/**
 * Ask the backend to reclaim space: sweep orphaned assets, return free pages, truncate the
 * journal.
 *
 * `documentId` is optional and the backend picks the current document when it is absent, so
 * the menu item does not need to know which document is open. Passing it explicitly is what
 * a test does, because "whatever is open" is not an assertion.
 *
 * The report distinguishes `assetsFreed` (content removed) from `pagesReclaimed` (space
 * returned to the filesystem). They are different numbers and only the first is a promise
 * about data: SQLite's `incremental_vacuum` only truncates pages at the *end* of a file, so
 * an asset freed from the middle is fully deleted and returns nothing. See
 * `optimize_document` in the shell for the measurement.
 */
export async function optimizeDocument(documentId?: string): Promise<OptimizeReport> {
  return invoke<OptimizeReport>('optimize_document', { documentId: documentId ?? null })
}

/**
 * Subscribe to native-menu commands.
 *
 * The menu is chrome *outside* the webview, so its items cannot call into the page directly.
 * It emits `holonomy://menu` and the page decides what to do, which is what keeps the menu
 * and the keyboard on one code path rather than two that drift.
 *
 * The id strings are the contract with `crates/holonomy-shell/src/menu.rs`, and that file's
 * `MenuCommand` enum is the single definition of them.
 */
export function onMenuCommand(handler: (command: MenuCommandId) => void): Promise<() => void> {
  return listen<MenuCommandId>('holonomy://menu', handler)
}

/** A command the native menu can emit. Mirrors `menu::MenuCommand` in the shell. */
export type MenuCommandId = 'optimize' | 'backup' | 'find'

/**
 * Export a document to PDF.
 *
 * # Why the bytes come back as well as being written
 *
 * Because the two callers want different things. A "Save as…" wants a path and knows nothing
 * more; a status line and a progress bar want the page count and the timings and do not care
 * where the file went. Returning 69MB over IPC to a caller that already wrote it would be
 * wasteful, and returning only a path would leave the caller unable to say how long the export
 * took or whether the translator dropped anything.
 *
 * So the bytes are always returned and writing is opt-in via `path`. A caller that wants neither
 * should not call this — see `holonomy-shell/src/export/pdf.rs` for the measured cost, which is
 * dominated by Typst's layout rather than by anything on this side.
 *
 * # Why `jobId` is a parameter
 *
 * Because it is the caller's name for this export, and three things depend on the frontend having
 * chosen it in advance: the progress subscription filters on it, `cancel_export` addresses it, and
 * a second export cannot overwrite the first's status line. Generating it here would work and
 * would leave the caller unable to act on any of those.
 *
 * # Why there is no progress *callback* parameter
 *
 * Because the reports arrive as an event, not as a return value — a 45-second command cannot hold
 * a response open and stream into it. `onExportProgress` below is the other half.
 */
export async function exportPdf(docId: string, jobId: string, path?: string): Promise<ExportReply> {
  return invoke<ExportReply>('export_pdf', { docId, jobId, path: path ?? null })
}

/**
 * Search the open document's text.
 *
 * No document id, unlike every other command here that takes one. The bridge command
 * reads the open document from core state instead, because search is the one query whose
 * *result set* is scoped by document: a caller-supplied id would let a search reach into a
 * document the user is not looking at.
 *
 * `limit` is a cap on what is *returned*, not on what is matched. The response carries
 * `total` separately so a truncated list can say so.
 */
export async function searchDocument(
  query: string,
  limit?: number,
): Promise<SearchResponse> {
  return invoke<SearchResponse>('search_document', { query, limit: limit ?? null })
}
