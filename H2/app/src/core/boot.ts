/**
 * The boot payload: what the frontend learns about a document before it renders.
 *
 * # Why the constants arrive from here rather than being written in the source
 *
 * The height model was duplicated: `GeometryCalibration::default` in Rust, and
 * three constants in `main.ts`, with a test asserting they matched. The
 * test worked, and it was still the wrong shape — a pair of hand-copied numbers
 * that must be kept in step by hand, failing at the worst possible moment
 * (silently, comparing a stale value against a fresh one after a typography
 * change). A parity test is an admission that the duplication was a mistake.
 *
 * So the constants come from the boot payload and there is nothing to keep in
 * step. This is the same reasoning as generating types rather than pairing them by
 * hand.
 *
 * # The wire format
 *
 * MessagePack, not JSON. The boot payload carries the calibration, the full
 * section manifest, and the contents of the sections near the caret — for a
 * 2000-page document that is a few hundred KB, and JSON parsing cost on the path
 * to first paint is the thing being avoided. The smaller, infrequent messages
 * (height syncs, lifecycle events) are JSON so they stay loggable.
 *
 * # One shape, defined once in Rust
 *
 * Every type below used to be hand-written here, mirroring Rust structs. Seven
 * shapes to keep in step by hand, and the two sides could drift with nothing
 * failing until a decode came back missing a field — the worst possible moment to
 * find out.
 *
 * They are generated from the Rust definitions in
 * `crates/holonomy-shell/src/bridge.rs` by `ts-rs`. A field rename on the Rust side
 * changes the generated file and any consumer still reading the old name stops
 * compiling, which is a build failure rather than a boot failure.
 *
 * `GeometryCalibration` comes from a second generated file, written by
 * `holonomy-core`: it is defined there, and the two crates cannot share one output
 * path because `ts-rs` overwrites rather than merges across writers.
 *
 * Decoding a payload is not here. It is in `geometry-bridge.ts`, next to the command
 * that produces one, because a MessagePack decoder sitting next to the types it
 * decodes is a second decoder waiting to disagree with the first — and this file
 * previously had one behind a `__HOLO_MP__` global that nothing ever installed.
 *
 * Regenerate with `scripts/gen-bridge-types.sh`.
 */

import type { GeometryCalibration } from './generated-calibration'
import type {
  BackupReply,
  OptimizeReport,
  BootPayload,
  CommitResponse,
  EphemeralDocument,
  ExportPhase,
  ExportProgress,
  ExportReply,
  HeightUpdate,
  LifecycleAction,
  LifecycleResult,
  ManifestSection,
  SearchHit,
  SearchResponse,
  SectionContent,
} from './generated-bridge'

// Re-exported rather than redeclared, so every consumer imports from one place and
// nothing in this file can disagree with Rust.
export type {
  BackupReply,
  OptimizeReport,
  BootPayload,
  CommitResponse,
  EphemeralDocument,
  ExportPhase,
  ExportProgress,
  ExportReply,
  GeometryCalibration,
  HeightUpdate,
  LifecycleAction,
  LifecycleResult,
  ManifestSection,
  SearchHit,
  SearchResponse,
  SectionContent,
}

/** The calibration, when the boot payload has not arrived.
 *
 * Not a set of constants — a marker. If anything needs a height before the
 * payload lands, that is a bug: the geometry must not render before it knows the
 * model. The throw is deliberate, because the alternative is silently using a
 * default that disagrees with the one every estimate is calibrated against.
 */
export function requireCalibration(cal: GeometryCalibration | null): GeometryCalibration {
  if (!cal) {
    throw new Error(
      'no height model loaded: geometry must not estimate before it knows the calibration. ' +
        'This is a boot-ordering bug, not a missing-constant problem.',
    )
  }
  return cal
}
