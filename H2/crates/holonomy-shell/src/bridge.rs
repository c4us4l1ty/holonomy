//! The Rust ⇄ TypeScript bridge contract.
//!
//! # Why this file exists
//!
//! Decision 6 of the bridge plan: payload structs are defined **once** in Rust and
//! the TypeScript types are generated from them. Not hand-written pairs, and not a
//! test asserting two files agree — both of which are a duplicated source of truth
//! wearing a disguise.
//!
//! `ts-rs` derives the TypeScript from the Rust definition, so a field rename
//! cannot leave the frontend reading a field that no longer exists: the generated
//! file changes, and any consumer that referenced the old name stops compiling.
//!
//! # What is generated, and what is deliberately not
//!
//! Generated, because it crosses the boundary and a mismatch is a runtime failure
//! nobody would enjoy debugging:
//!
//! - [`BootPayload`] — the boot document
//! - [`ManifestSection`] — one row of the frozen layer
//! - [`SectionContent`] — a section's compressed bytes
//! - [`GeometryCalibration`] — re-exported from `holonomy_core` so the frontend's
//!   copy of the constants has the same definition as the source
//! - [`HeightUpdate`], [`LifecycleAction`] — the small JSON messages
//!
//! Not generated: anything the frontend does not receive. The editor's own types
//! live in TypeScript because they are shaped by Tiptap, not by us.
//!
//! # Regenerating
//!
//! ```sh
//! cargo test -p holonomy-shell bridge_types
//! ```
//!
//! That test writes `app/src/core/generated-bridge.ts` and asserts it matches what
//! is on disk, so a stale generated file fails the suite rather than being
//! discovered at runtime. It is a *staleness* guard, not a parity guard: the
//! content is derived, so there is nothing to keep in step by hand.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Pixels per 100 characters, per block, and per-section chrome.
///
/// Re-exported from `holonomy_core` so the generated TypeScript comes from the
/// single definition. The frontend used to keep its own copy of these three
/// numbers, guarded by a test asserting they matched; that arrangement is gone
/// (DOCTRINE.md §8).
pub use holonomy_core::geometry::GeometryCalibration;

/// One row of the section manifest — the whole document as far as anything outside
/// the focused window is concerned.
///
/// ~100 bytes per section, so a 2000-page document is ~50KB and the scrollbar,
/// outline and word counts are all cheap. Carries no content: that is the point.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../app/src/core/generated-bridge.ts")]
pub struct ManifestSection {
    pub id: String,
    /// Fractional index: a u64 with gaps, not a digit string. See
    /// `holonomy_core::order` for why bare fractional digits are not a total
    /// order.
    ///
    /// Typed as `number` rather than ts-rs's default `bigint` for `u64`, because that
    /// default is *wrong here* rather than merely conservative.
    ///
    /// MessagePack has one unsigned integer type, and `@msgpack/msgpack` decodes it to
    /// a JS `number` whenever the value fits in 53 bits -- which every real key does.
    /// A declared `bigint` would then be a claim the wire does not honour, and the
    /// first arithmetic on the field would be a type error against a `number` at
    /// runtime. A type that says `bigint` over a value that is a `number` is worse
    /// than either being consistent.
    ///
    /// The precision hazard is real but bounded, and bounded *by a test*:
    /// `order::wire_precision_tests::order_keys_stay_exactly_representable` asserts
    /// that the keys for four billion sections still fit in 53 bits. Reaching the
    /// limit takes about 8.8 trillion sections; a 2000-page document is 667.
    #[ts(type = "number")]
    pub order_key: u64,
    pub title: Option<String>,
    pub word_count: u32,
    pub mark_count: u32,
    pub char_count: u32,
    /// Top-level block count. Not optional and not defaulted anywhere: it is the
    /// difference between a height estimate accurate to ~5% and one 225% out, and
    /// every write path in `holonomy_core` populates it.
    pub block_count: u32,
    /// Epoch milliseconds. `number`, not `bigint` -- see the note on `order_key`.
    ///
    /// ## Why the timestamps are `number` and not `bigint`
    ///
    /// Same reason as `order_key`: `rmp-serde` writes an `i64` as a MessagePack
    /// 64-bit integer, and `@msgpack/msgpack` decodes it to a JS `number` whenever it
    /// fits in 53 bits. Epoch milliseconds do -- 2^53 milliseconds is about 285,000
    /// years -- so `bigint` would be a type the wire does not deliver.
    ///
    /// The bound is asserted rather than argued: `order::wire_precision_tests`
    /// covers both the keys and the timestamps, because a contract whose numeric
    /// types depend on a range nobody checks is a contract that breaks silently.
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number")]
    pub updated_at: i64,
}

/// Everything the frontend needs for its first frame.
///
/// Sent as **MessagePack**, not JSON. This payload carries the manifest plus the
/// content of every section near the caret; for a 2000-page document that is a few
/// hundred KB on the path to first paint, and JSON parse cost there is exactly what
/// the boot sequence is trying to avoid.
///
/// Everything *after* boot is JSON, because those messages are small and being
/// able to read one in a log is worth more than the bytes.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../app/src/core/generated-bridge.ts")]
pub struct BootPayload {
    pub document_id: String,
    pub title: String,
    /// The height model, fitted by `app/test/calibrate.ts`. Shipped rather than
    /// compiled into the frontend so there is one definition and the packaged app
    /// cannot disagree with the measurements it was fitted from.
    pub calibration: GeometryCalibration,
    pub sections: Vec<ManifestSection>,
    /// Sections whose content is included. The rest of the document is manifest
    /// rows only, which is what lets the scrollbar be correct before anything is
    /// mounted.
    pub visible: Vec<SectionContent>,
    /// Where the caret was when the document was last closed, if known.
    pub focused_section_id: Option<String>,
    /// Scroll offset to restore, in pixels. Null on first open.
    pub scroll_top: Option<f64>,
}

/// One section's content, fetched on demand.
///
/// The same shape as the boot payload's `visible` entries, deliberately: a section
/// fetched later must be indistinguishable from one fetched at boot, or the frontend
/// would need two decode paths and one of them would go untested until a user scrolled
/// past the boot window.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../app/src/core/generated-bridge.ts")]
pub struct SectionContent {
    pub id: String,
    /// zstd-compressed ProseMirror JSON.
    ///
    /// # `serde_bytes` is load-bearing, and its absence was a real bug
    ///
    /// `rmp-serde` serialises a plain `Vec<u8>` as a MessagePack **array of numbers**,
    /// not as a MessagePack `bin`. `@msgpack/msgpack` therefore decoded it to
    /// `number[]`, and both frontend decoders — which check
    /// `instanceof Uint8Array` and throw rather than repair — refused it.
    ///
    /// So the boot payload's content had *never* decoded. It went unnoticed for a
    /// specific and unpleasant reason: `app/test/boot.ts` encodes its own fixture with
    /// `@msgpack/msgpack`'s `encode`, which writes a `Uint8Array` as `bin`. The test
    /// therefore agreed with itself and with neither the real encoder nor the real
    /// decoder. The in-engine verification found it, on the first run where a payload
    /// carrying section content came off the bridge — and the message it produced named
    /// the symptom precisely while the cause was three layers down.
    ///
    /// `serde_bytes` makes `rmp-serde` emit `bin`. It is also what makes the choice of
    /// MessagePack worth having: a 7KB section is 7KB on the wire as `bin`, and roughly
    /// 30KB as an array of small integers — which is larger than the JSON it replaced,
    /// defeating the entire argument for a binary format on this one field.
    ///
    /// `#[serde(with = ...)]` and `#[ts(type = ...)]` are both needed and say different
    /// things: the first is what the encoder does, the second is what the generated
    /// TypeScript says. A type annotation cannot change a wire format, and it was the
    /// `#[ts]` half that existed when this broke.
    #[serde(with = "serde_bytes")]
    #[ts(type = "Uint8Array")]
    pub content_zstd: Vec<u8>,
}

/// A throwaway document, built in the store for the verification harness.
///
/// # Why this is a command and not a fixture
///
/// The LRU bound is only meaningful against a document whose content lives in SQLite,
/// because that is the only case where dropping a section's bytes is *recoverable*. A
/// synthetic document supplied by the frontend has nothing to fetch from, so the cache
/// correctly declines to drop it — which is why the in-engine run could not exercise
/// the bound at all until this existed. The bound was Node-tested and in-engine
/// untested, and the two are not substitutes: the Node test proves the data structure
/// evicts, and only this proves the wiring does.
///
/// # Why it is ephemeral
///
/// It is built in the user's real database. Fifty sections of fixture prose left behind
/// by every cross-engine run would be litter next to real work, so the harness deletes
/// it with `delete_ephemeral_document` when it is done.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../app/src/core/generated-bridge.ts")]
pub struct EphemeralDocument {
    pub document_id: String,
    pub section_ids: Vec<String>,
    /// Sections whose content the boot payload will carry, and how many there are in all.
    ///
    /// Both, so the harness can assert that the ones past the boot window really did
    /// arrive without content. A test that cannot tell the two apart would pass against a
    /// payload that carried everything.
    pub boot_visible: usize,
    pub sections: usize,
}

/// What `commit_section_edit` did, and the counts it settled on.
///
/// # Why the counts come back
///
/// Because the caller supplied some of them and Rust recomputed the rest, and a caller
/// that cannot see the authoritative values will keep using its own. A section's
/// `block_count` drives its height estimate, so a frontend and a store that disagree
/// about it produce a scrollbar that is wrong for that section for the rest of the
/// session, with no error anywhere.
///
/// Returning them makes the divergence visible at the moment it happens and lets the
/// caller correct its record rather than accumulate a quiet difference.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../app/src/core/generated-bridge.ts")]
pub struct CommitResponse {
    pub section_id: String,
    /// Row id in the write-ahead log, for diagnostics and for the WAL's own tests.
    ///
    /// `number`, not `bigint` — the same reason as `order_key`: an `i64` crossing
    /// MessagePack arrives as a JS number inside 53 bits, which a row counter always is.
    #[ts(type = "number")]
    pub wal_row_id: i64,
    /// Counts derived by `analyze` from the bytes actually written.
    pub word_count: u32,
    pub char_count: u32,
    pub block_count: u32,
    /// Echoed back rather than derived: `analyze` has no mark counter.
    pub mark_count: u32,
    /// Un-freed WAL rows for this document after this write.
    ///
    /// Reported because it is the number that says whether the WAL is being folded. A
    /// frontend can watch it climb and knows something is wrong long before the file
    /// does.
    pub pending: u32,
}

/// One measured height, batched and debounced before crossing the bridge.
///
/// A `ResizeObserver` can fire many times per second per section while the user
/// types. Batching and debouncing is what keeps that off the IPC path.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../app/src/core/generated-bridge.ts")]
pub struct HeightUpdate {
    pub section_id: String,
    /// Position in document order. Carried alongside the id because the geometry is
    /// index-keyed, and resolving an id to an index on every batch would be a
    /// lookup per update for no benefit.
    pub index: u32,
    /// Measured height in CSS pixels. Fractional, because
    /// `getBoundingClientRect` is and rounding here would show as jitter.
    pub height: f64,
}

/// A structural change to the document.
///
/// Serialised as a tagged enum so the frontend can switch on `kind` without
/// guessing from which fields are present.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export, export_to = "../../../app/src/core/generated-bridge.ts")]
pub enum LifecycleAction {
    /// A section grew past a threshold and was divided.
    ///
    /// `at_block` is the index within the section where the cut falls, so the
    /// storage layer can rebalance order keys without re-reading the content.
    Split {
        section_id: String,
        at_block: u32,
        index: u32,
    },
    /// A section shrank below a threshold and joined its predecessor.
    Merge {
        section_id: String,
        into_section_id: String,
        index: u32,
    },
    /// An empty section was removed outright.
    ///
    /// # Why this is a third variant rather than a `Merge` with no target
    ///
    /// A merge moves the emptied section's content into a neighbour and keeps both rows
    /// involved: the target is re-bodied and the source survives as an empty shell.
    /// A prune deletes the row. The store's work is genuinely different — a merge
    /// rewrites one section's blob, a prune issues a `DELETE` and must also drop the
    /// section's search-index row, which the cascade does not cover because
    /// `sections_fts` is a separate virtual table.
    ///
    /// Overloading `Merge` with an empty `into_section_id` would have worked and been
    /// unreadable: `merge_section` would have had to branch on the emptiness of a string
    /// that is typed `String`, so the invalid state would be representable and only
    /// caught at runtime.
    ///
    /// `previous_section_id` is retained so the store can verify adjacency. The frontend
    /// decides *that* the section is empty and asks for it to go; the store re-checks that
    /// the section really is empty before deleting it, because the two can disagree about
    /// what the document contains and a wrong deletion is not recoverable.
    Prune {
        section_id: String,
        previous_section_id: String,
        index: u32,
    },
}

/// The reply to a lifecycle action.
///
/// Reports what actually happened rather than assuming the request succeeded: a
/// split that could not find a valid cut point comes back with
/// `applied: false`, and the frontend leaves the document as it is rather than
/// optimistically reconciling against a split that did not occur.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../app/src/core/generated-bridge.ts")]
pub struct LifecycleResult {
    pub applied: bool,
    /// Section ids in the new order, when applied. The frontend replaces its
    /// ordering wholesale rather than patching, because a split can move keys on
    /// both sides of the cut.
    pub section_ids: Vec<String>,
    /// Why nothing happened, when `applied` is false.
    pub reason: Option<String>,
}

// -- search ------------------------------------------------------------------

/// One section that matched a search, with enough context to show it.
///
/// Section-level, not match-level, and that is a deliberate limit rather than a missing
/// feature. `holonomy-core`'s `SearchHit` is also section-level; the two are one row per
/// section, not per occurrence. What a user wants from a find is "which section is this
/// in and what does it say", and the frontend's job is to take them there.
///
/// The occurrence *offset* is absent because the contentless FTS5 index cannot produce
/// one: it stores tokens, not character positions, and FTS5 exposes
/// `highlight()`/`snippet()` offsets for a contentless table over its own column -- which
/// is exactly the text this schema stopped storing twice. The frontend therefore places
/// the caret by searching the section's own text for the query, which it has anyway.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../app/src/core/generated-bridge.ts")]
pub struct SearchHit {
    /// The section to navigate to. The frontend scrolls this into the window and focuses
    /// it; it does not need to know the section's position in the document, because the
    /// geometry is the only thing that knows that and it already does.
    pub section_id: String,
    /// Context around the match with `<b>` around the matched text.
    ///
    /// HTML, not a list of runs. FTS5's own `snippet()` emits HTML for the same purpose
    /// and did so before the contentless move replaced it; a structured
    /// `{before, match, after}` triple would be marginally safer to render and costs a
    /// second type on the bridge for a string the backend already formatted.
    pub snippet: String,
    /// bm25 relevance; lower is better.
    pub score: f64,
}

/// The result of a search, with enough context for the UI to explain itself.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../app/src/core/generated-bridge.ts")]
pub struct SearchResponse {
    pub hits: Vec<SearchHit>,
    /// How many sections matched in total, before `limit` was applied.
    ///
    /// Carried because "showing 20 of 340" and "showing all 20" are different
    /// situations, and a truncated list that does not say so reads as a complete answer.
    pub total: usize,
}

// The staleness checks for these types are an *integration* test, in
// `crates/holonomy-shell/tests/bridge-contract.rs`.
//
// They started here, as unit tests in this module, and failed for a reason worth
// recording: `#[ts(export)]` makes `ts-rs` generate one `#[test]` per type, all
// named `bridge::export_bindings_*`. A filter of `bridge::` matches those too, so
// cargo ran the writers and the staleness check concurrently, in the same process,
// against the same file. The check read the file mid-rewrite and reported that
// `BootPayload` was missing from a file that contained `BootPayload` on disk.
//
// Both were in the same module and the filter matched both; moving the check to a
// different test binary puts it in a different process, where it cannot race.

// -- document files -----------------------------------------------------------

/// One document in the open file, for a switcher.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../app/src/core/generated-bridge.ts")]
pub struct DocumentSummary {
    pub id: String,
    pub title: String,
    /// Section count, so the switcher can show size without a manifest read per row.
    pub sections: u32,
    /// Words, likewise.
    pub words: u32,
    #[ts(type = "number")]
    pub updated_at: i64,
}

/// What opening a path did.
///
/// # Why three outcomes and not a bool
///
/// Because the failure a user can hit is not an exception — it is "that file is a database
/// but not a document", and "there was nothing at that path, so a new document was
/// created". Those need different messages and the frontend cannot tell them apart from a
/// success. A bool would force it to re-derive the distinction from an error it may not
/// have been given, which is exactly the sort of inference that disagrees with the store on
/// a machine nobody tested.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../app/src/core/generated-bridge.ts")]
pub struct OpenDocumentReport {
    pub document: DocumentSummary,
    /// Where the document lives, so the title bar and "Save As…" can name it.
    pub path: String,
    /// `existing` | `created` | `empty`.
    ///
    /// `empty` is its own case because an openable `.holo` with no documents in it is a
    /// legitimate thing — a backup of an empty file, or a store the user has since cleared —
    /// and reporting "opened your document" would be true and useless.
    pub kind: String,
}
