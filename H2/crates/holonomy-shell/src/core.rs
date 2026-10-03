//! The bridge's three commands, as pure Rust over a store.
//!
//! # Why the logic is here and not in `lib.rs`
//!
//! Everything a command needs to *decide* lives in this module, and the
//! `#[tauri::command]` wrappers in `lib.rs` only translate. The reason is
//! testability under a constraint this project has run into repeatedly: the
//! webkit2gtk window cannot be driven from a test on a headless Wayland seat
//! without a compositor, so anything reachable only through a command can only be
//! tested by hand, in a window, once.
//!
//! Every function here takes `&Store` and returns a plain value or a
//! [`Result`](holonomy_core::Result). `get_document_boot`, `sync_section_heights`
//! and `commit_section_lifecycle` are therefore all covered by ordinary unit
//! tests against an in-memory store, and the IPC layer is left holding only the
//! serialisation, which is the part that has no interesting logic.
//!
//! # What the store already guarantees
//!
//! `holonomy-core` has 98 tests behind it, and this module leans on them rather
//! than re-deriving: `manifest()` is the only read needed to display a 2000-page
//! document, `section_bytes` returns stored bytes verbatim, `block_count` is
//! recomputed from the JSON on every write, and order keys are `u64` with 1024-wide
//! gaps. None of that is repeated here.
//!
//! # The one thing that does not come from the store
//!
//! Heights. The store knows word, character and block counts; it does not know how
//! tall a section renders, because that depends on the engine's text layout. So the
//! height model lives in [`Geometry`], seeded from the manifest at boot and
//! corrected by `sync_section_heights` as the frontend measures real sections.

use holonomy_core::error::Error;
use holonomy_core::geometry::GeometryCalibration;
use holonomy_core::{analyze, Geometry, ManifestEntry, Result, SectionMetrics, Store};
use serde_json::{json, Value};
use ts_rs::TS;

use crate::bridge::{
    BootPayload, CommitResponse, LifecycleAction, LifecycleResult, ManifestSection, SearchHit,
    SearchResponse, SectionContent,
};

/// Sections whose content the boot payload carries.
///
/// # Why twelve
///
/// A 1500-word section renders to roughly 3400px at the measured calibration, so a
/// 1080px window shows about a third of one. Twelve sections is four screens: enough
/// that a fast scroll cannot outrun the content, small enough that the payload stays
/// in the low hundreds of kilobytes even at the compression ratio M1 measured
/// (12KB to 7KB for a full section).
///
/// It is a constant rather than a viewport calculation because the boot payload is
/// assembled before the webview reports its size, and because a payload that varies
/// with window height is a payload that is wrong on the second window. The frontend
/// asks for more as it scrolls; this is the floor, not the ceiling.
pub const BOOT_VISIBLE_SECTIONS: usize = 12;

/// Build the payload the frontend needs for its first frame.
///
/// # The four decisions in here
///
/// 1. **Which document.** The one named, or the most recently updated, or a new one.
///    "Most recently updated" rather than "first" because a second window on the
///    same app should reopen what the user was working in.
/// 2. **WAL recovery first.** `recover()` folds any unflushed rows into their
///    section blobs, so a crash loses at most the debounce window. Before this, a
///    section's manifest counts describe the last *flush*, not what is on screen —
///    and the geometry is built from those counts, so a stale manifest means a
///    wrong scrollbar before the user has typed anything.
/// 3. **Content for the first twelve sections only.** The manifest alone is enough
///    for the scrollbar; content is needed for what is on screen.
/// 4. **Heights are seeded from counts.** Every section starts at its estimated
///    height, and `sync_section_heights` replaces estimates with measurements as
///    they arrive.
pub fn get_document_boot(store: &Store, document_id: Option<&str>) -> Result<BootPayload> {
    let document_id = match document_id {
        Some(id) => id.to_string(),
        None => most_recent_document(store)?,
    };

    // Crash recovery, before anything reads the manifest. See above.
    store.recover(&document_id)?;

    // Bring the search index up to date if it is not. Every edit path maintains it, so
    // this is expected to find nothing to do -- and it is here for the cases where that
    // is not true: a document created before the index existed, a file migrated from
    // schema 1 (whose index was the external-content one and was dropped on the way), or
    // an index left behind by a failed write.
    //
    // The check is a count comparison rather than an unconditional rebuild. A rebuild of
    // a 1,300-section document re-tokenises 1.33M words, which is milliseconds on a small
    // file and long enough to be felt on a large one; paying that on every open of every
    // document to guard against a case that is rare is the wrong trade. `index_gap`
    // is one indexed B-tree walk against one indexed count, and it answers the only
    // question that matters: is anything missing.
    //
    // Note what this *cannot* repair, because it is worth being explicit: an index with
    // the right number of rows and the wrong contents. That would need a rebuild on
    // every open, and nothing in the write paths produces one -- each index write
    // replaces its row in the same statement sequence as the section write it follows.
    let (indexed, sections) = store.index_gap(&document_id)?;
    if indexed < sections {
        store.reindex(&document_id)?;
    }

    let manifest = store.manifest(&document_id)?;
    let document = store.document(&document_id)?;

    let sections: Vec<ManifestSection> = manifest
        .entries()
        .iter()
        .map(manifest_section)
        .collect();

    let visible_ids: Vec<String> = sections
        .iter()
        .take(BOOT_VISIBLE_SECTIONS)
        .map(|s| s.id.clone())
        .collect();
    let visible: Vec<SectionContent> = store
        .section_bytes_many(&visible_ids)?
        .into_iter()
        .map(|(id, content_zstd)| SectionContent { id, content_zstd })
        .collect();

    Ok(BootPayload {
        document_id: document.id,
        title: document.title,
        // The calibration travels with the boot payload rather than being compiled
        // into the frontend, so the packaged app cannot disagree with the model
        // every estimate is measured against. `default()` is the fitted value, not
        // a placeholder: `app/test/calibrate.ts` fits it against real rendered
        // sections and `emit-calibration` writes it to `calibration.json`.
        calibration: GeometryCalibration::default(),
        sections,
        visible,
        // No session table exists yet, so there is nothing to restore a caret from.
        // Null rather than a guess: the frontend treats null as "start at the top",
        // and a wrong guess would scroll a 2000-page document somewhere arbitrary
        // with no way for the user to tell it was wrong.
        focused_section_id: None,
        scroll_top: None,
    })
}

/// The document to open when the frontend names none.
///
/// Most recently updated, creating one if the store is empty. "Empty" is the first
/// run, and a fresh document with a single empty section is what makes the
/// frontend's "no sections" path reachable at all — worth keeping real.
fn most_recent_document(store: &Store) -> Result<String> {
    let docs = store.documents()?;
    if let Some(d) = docs.iter().max_by_key(|d| d.updated_at) {
        return Ok(d.id.clone());
    }
    let doc = store.create_document("Untitled")?;
    store.add_section(&doc.id, &empty_doc())?;
    Ok(doc.id)
}

/// A valid empty ProseMirror document.
///
/// `{ "type": "doc", "content": [] }` is what ProseMirror itself produces for an
/// empty doc, and it is what `add_section` needs to store something the renderer
/// can mount. Not a doc with one empty paragraph: that is the *frontend's* choice
/// about what an empty section looks like, and deciding it here would mean two
/// places that disagree about what an empty section is.
fn empty_doc() -> Value {
    serde_json::json!({ "type": "doc", "content": [] })
}

/// One manifest row, as the frontend sees it.
///
/// A conversion rather than a shared type, and the reason is worth stating:
/// `ManifestEntry.order_key` is an `OrderKey` newtype, which serialises as a bare
/// number and carries the invariant that keys are ordered. Widening it to `u64` here
/// is where that invariant stops mattering to the frontend — and that is correct,
/// because the frontend sorts by array position and never by key.
fn manifest_section(e: &ManifestEntry) -> ManifestSection {
    ManifestSection {
        id: e.id.clone(),
        order_key: e.order_key.0,
        title: e.title.clone(),
        word_count: e.word_count,
        mark_count: e.mark_count,
        char_count: e.char_count,
        block_count: e.block_count,
        created_at: e.created_at,
        updated_at: e.updated_at,
    }
}

/// Fold a batch of measured heights into the geometry.
///
/// # Why the return value is a total and not a list of deltas
///
/// The frontend needs one number: how tall the document is now. It then decides
/// whether to compensate the scroll position, and `scroll_compensation` — which
/// takes the index, the delta and the viewport top — is already the frontend's
/// rule, mirrored in `local-geometry.ts`. Returning per-section deltas would mean
/// sending the frontend everything it needs to *recompute* a decision it should not
/// be recomputing. So: heights in, total out, and the correction logic stays on the
/// side that owns the viewport.
///
/// # The `len` check
///
/// A geometry of a different length than the batch is a frontend and a store that
/// disagree about how many sections the document has — which happens transiently
/// during a split, and permanently if something is wrong. Rather than silently
/// writing past the end or applying offsets to the wrong sections, the batch is
/// rejected and the frontend keeps the total it had. A wrong scrollbar that
/// recovers on the next batch beats an index-out-of-bounds.
pub fn sync_section_heights(
    geometry: &mut Geometry,
    updates: &[crate::bridge::HeightUpdate],
) -> Result<f64> {
    if !updates.is_empty() && updates.iter().any(|u| u.index as usize >= geometry.len()) {
        return Err(Error::Other(anyhow::anyhow!(
            "height batch of {} sections does not fit a geometry of {} sections; \
             the frontend and the store disagree on the section count",
            updates.len(),
            geometry.len()
        )));
    }
    for u in updates {
        geometry.update_measured_height(u.index as usize, u.height);
    }
    Ok(geometry.total_height())
}

/// Apply a structural change: a split or a merge.
///
/// # Both operations end the same way
///
/// Re-read the manifest and return the whole section ordering. A split moves keys
/// on both sides of the cut, and a merge removes one; in both cases a patch
/// describing only what changed has to be exactly right about what did not, and the
/// frontend would then be reconciling a document shape against a diff. A 667-entry
/// id array is about 27KB, and it is sent once per structural change rather than
/// per keystroke.
///
/// # Why `index` from the frontend is treated as a hint
///
/// The frontend knows where the section is on screen. The store knows where it is in
/// the ordering. `locate_section` re-derives the index from the store and the
/// returned ordering uses *that*, so a frontend and store that disagree produce a
/// correct ordering and a fresh boot payload rather than a silently misordered
/// document.
pub fn commit_section_lifecycle(
    store: &Store,
    geometry: &mut Geometry,
    action: &LifecycleAction,
) -> Result<LifecycleResult> {
    match action {
        LifecycleAction::Split {
            section_id, at_block, index,
        } => split_section(store, geometry, section_id, *at_block, *index),
        LifecycleAction::Merge {
            section_id,
            into_section_id,
            index,
        } => merge_section(store, geometry, section_id, into_section_id, *index),
        LifecycleAction::Prune {
            section_id,
            previous_section_id,
            index,
        } => prune_section(store, geometry, section_id, previous_section_id, *index),
    }
}

/// Remove an empty section, re-verifying that it is empty.
///
/// # Why the emptiness check is repeated here
///
/// The frontend decides the section is empty, from the live editor, and asks for it to be
/// removed. By the time the request arrives the editor may have been edited further, the
/// user may have typed into another section while it was in flight, and the frontend's
/// copy of the section may simply be stale — it is behind an IPC round trip.
///
/// Deleting a section that has content is not recoverable from the editor, because the
/// registry removes the section and destroys the editor in the same gesture. So the store
/// checks the stored content itself and refuses. `applied: false` with a reason, exactly
/// like a split that cannot find a cut: the frontend asked for something the document's
/// state does not permit, and that is not an error.
///
/// # Why adjacency is verified rather than trusted
///
/// `previous_section_id` exists so the store can confirm the section being removed is the
/// one the user was standing in front of. Removing some *other* empty section would leave
/// the empty one the user is looking at still on screen, so the gesture would appear to do
/// nothing while a different section silently disappeared.
/// Is this section's content exactly one empty paragraph?
///
/// # Why this exists rather than a count comparison
///
/// `analyze` reports `block_count == 1` for a section holding one image, because an
/// image occupies its own line box and the height model is calibrated on that. So the
/// natural check — "one block, no words, no characters" — calls a picture an empty
/// section and prunes away a figure the user inserted. `LocalGeometry`'s mirror of this
/// rule, `isSectionEmpty` in the frontend, walks the blocks for the same reason.
///
/// # What counts as blank
///
/// A single `paragraph` with no `content`, or whose content is only empty text nodes and
/// a hard break. Whitespace-only text is blank, because a paragraph of spaces is empty
/// to the user and selecting it selects the whole line.
///
/// Anything else at the top level — an image, a rule, a table, a second paragraph — is
/// content. The section may have had one block deleted, but it is not blank.
fn section_is_blank_paragraph(json: &Value) -> bool {
    let Some(content) = json.get("content").and_then(Value::as_array) else {
        return false;
    };
    // Exactly one top-level block, or a section with two empty paragraphs is "empty" by
    // any user-facing measure but is not what this gesture removes.
    if content.len() != 1 {
        return false;
    }
    let Some(block) = content[0].as_object() else {
        return false;
    };
    if block.get("type").and_then(Value::as_str) != Some("paragraph") {
        return false;
    }
    match block.get("content") {
        None | Some(Value::Null) => true,
        Some(Value::Array(items)) => items.iter().all(|item| match item {
            Value::Object(o) => {
                o.get("type").and_then(Value::as_str) == Some("text")
                    && o.get("text")
                        .and_then(Value::as_str)
                        .map(|t| t.trim().is_empty())
                        .unwrap_or(true)
                    || o.get("type").and_then(Value::as_str) == Some("hardBreak")
            }
            _ => false,
        }),
        Some(_) => false,
    }
}

fn prune_section(
    store: &Store,
    geometry: &mut Geometry,
    section_id: &str,
    previous_section_id: &str,
    _hint_index: u32,
) -> Result<LifecycleResult> {
    let (document_id, index) = store.locate_section(section_id)?;
    let (previous_document, previous_index) = store.locate_section(previous_section_id)?;

    if document_id != previous_document {
        return Ok(LifecycleResult {
            applied: false,
            section_ids: Vec::new(),
            reason: Some("cannot prune a section against a section in another document".into()),
        });
    }
    if section_id == previous_section_id {
        return Ok(LifecycleResult {
            applied: false,
            section_ids: Vec::new(),
            reason: Some("cannot prune a section against itself".into()),
        });
    }
    if previous_index + 1 != index {
        return Ok(LifecycleResult {
            applied: false,
            section_ids: Vec::new(),
            reason: Some(format!(
                "{previous_section_id} is not immediately before {section_id}, so they are not a seam"
            )),
        });
    }

    // The re-check. `analyze` is the same definition the height model is calibrated
    // against, so "empty" here means empty by the project's own measure rather than by
    // whatever the frontend's word-counting happened to produce.
    //
    // `block_count` is what catches a figure, and it is not enough on its own: an image
    // is one block, so a section holding a single image reports `block_count == 1` and
    // passes a `<= 1` test. `analyze` counts an image as a block because it occupies its
    // own line box, which is exactly right for the height model and exactly wrong here.
    //
    // So the text checks run first and `block_count` only ever *adds* suspicion: a section
    // is empty when it has no words, no characters, and no block that is not an empty
    // paragraph. Anything else — an image, a rule, an equation, a second paragraph — is
    // content the user put there, and the error of deleting it is not recoverable.
    let stored = store.load_section(section_id)?;
    let analyzed = holonomy_core::analyze(&stored);
    let is_empty = analyzed.word_count == 0
        && analyzed.char_count == 0
        && section_is_blank_paragraph(&stored);
    if !is_empty {
        return Ok(LifecycleResult {
            applied: false,
            section_ids: Vec::new(),
            reason: Some(format!(
                "{section_id} holds {} word(s) and {} block(s), so it is not empty",
                analyzed.word_count, analyzed.block_count
            )),
            // The report names the block, because "not empty" is not actionable on its
            // own and the frontend logs this verbatim.
        });
    }

    // `delete_section` drops the search-index row as well as the section, so the removed
    // section stops being findable instead of becoming a search hit that navigates to
    // nothing.
    store.delete_section(section_id)?;

    rebuild_geometry(store, geometry, &document_id)?;

    Ok(LifecycleResult {
        applied: true,
        section_ids: store.section_ids(&document_id)?,
        reason: Some(format!("pruned empty section {section_id}")),
    })
}

/// Divide one section into two at a block boundary.
///
/// # Where the cut actually lands
///
/// `at_block` is the frontend's suggestion and is clamped into `1..len-1`. A cut at
/// block 0 would leave the original empty, and a cut at the end would leave the new
/// section empty; both produce a document that is technically valid and practically
/// broken — an empty section the user has to scroll past, forever. So the request is
/// honoured where it can be and refused where it cannot.
///
/// A section with fewer than two blocks cannot be split at all, and says so rather
/// than creating an empty section. That is `applied: false` and a reason, not an
/// error: the frontend asked for something the document's shape does not allow, and
/// an error would read as a crash.
fn split_section(
    store: &Store,
    geometry: &mut Geometry,
    section_id: &str,
    at_block: u32,
    _hint_index: u32,
) -> Result<LifecycleResult> {
    let (document_id, index) = store.locate_section(section_id)?;
    let json = store.load_section(section_id)?;
    let content = json
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Corrupt {
            section_id: section_id.to_string(),
            reason: "document node has no content array".into(),
        })?;

    if content.len() < 2 {
        return Ok(LifecycleResult {
            applied: false,
            section_ids: Vec::new(),
            reason: Some(format!(
                "section has {} block(s) and cannot be split; a split needs at least 2",
                content.len()
            )),
        });
    }

    let cut = (at_block as usize).clamp(1, content.len() - 1);

    let head = Value::Array(content[..cut].to_vec());
    let tail = Value::Array(content[cut..].to_vec());

    let before = store.manifest(&document_id)?;
    let keys: Vec<u64> = before.entries().iter().map(|e| e.order_key.0).collect();
    let insert_at = index + 1;
    let new_key = match holonomy_core::order::key_for_insert(insert_at, &keys) {
        Ok(k) => k,
        // The gap between two keys has closed. `rebalance_existing` is the answer,
        // and falling back to it rather than surfacing an error is the right call:
        // a failed insert is a document that cannot grow, and rebalancing 667 keys
        // is a few microseconds.
        //
        // The rebalanced keys are *not* written back here. They are only used to
        // find one key with room around it, and writing them would mean rewriting
        // every section's key to fix an insert that a single key could solve.
        // `rebalance_existing` returning `None` means the keys are not strictly
        // increasing, which is a corrupt manifest rather than a full one — that does
        // deserve an error.
        Err(_) => {
            let fresh = holonomy_core::order::rebalance_existing(&keys).ok_or_else(|| {
                Error::Other(anyhow::anyhow!(
                    "order keys are not strictly increasing, so no insert is possible; \
                     the manifest is inconsistent rather than merely full"
                ))
            })?;
            holonomy_core::order::key_for_insert(insert_at, &fresh)?
        }
    };

    let head_metrics = SectionMetrics::new(0, 0, 0);
    store.save_section(section_id, &serde_json::json!({"type":"doc","content":head}), head_metrics, "")?;
    let new_id = store.insert_section(&document_id, new_key, &serde_json::json!({"type":"doc","content":tail}))?;

    rebuild_geometry(store, geometry, &document_id)?;

    Ok(LifecycleResult {
        applied: true,
        section_ids: store.section_ids(&document_id)?,
        reason: Some(format!("split {section_id} at block {cut} into {new_id}")),
    })
}

/// Fold `section_id` into `into_section_id` and delete it.
///
/// The merged content is written to the *target*, so the id the frontend was
/// focusing survives. The other direction would move the caret's section out from
/// under it, and the frontend would have to re-resolve focus after every merge.
///
/// Concatentating rather than re-deriving: the two sections are adjacent top-level
/// block lists, so the merged document is their concatenation. Nothing is
/// recomputed except what `save_section` always recomputes.
fn merge_section(
    store: &Store,
    geometry: &mut Geometry,
    section_id: &str,
    into_section_id: &str,
    _hint_index: u32,
) -> Result<LifecycleResult> {
    let (document_id, _) = store.locate_section(section_id)?;
    let (into_document, _) = store.locate_section(into_section_id)?;

    if document_id != into_document {
        return Ok(LifecycleResult {
            applied: false,
            section_ids: Vec::new(),
            reason: Some("cannot merge sections from different documents".into()),
        });
    }
    if section_id == into_section_id {
        return Ok(LifecycleResult {
            applied: false,
            section_ids: Vec::new(),
            reason: Some("cannot merge a section into itself".into()),
        });
    }

    let source = store.load_section(section_id)?;
    let target = store.load_section(into_section_id)?;

    let mut content: Vec<Value> = target
        .get("content")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if let Some(tail) = source.get("content").and_then(Value::as_array) {
        content.extend(tail.iter().cloned());
    }

    store.save_section(
        into_section_id,
        &serde_json::json!({"type":"doc","content":content}),
        SectionMetrics::new(0, 0, 0),
        "",
    )?;
    store.delete_section(section_id)?;

    rebuild_geometry(store, geometry, &document_id)?;

    Ok(LifecycleResult {
        applied: true,
        section_ids: store.section_ids(&document_id)?,
        reason: Some(format!("merged {section_id} into {into_section_id}")),
    })
}

/// Rebuild the geometry from the store's manifest after a structural change.
///
/// From scratch rather than surgically: a split adds one section and shifts every
/// index above it, a merge removes one and shifts everything below, and keeping a
/// `Geometry` correct across either means re-deriving indices anyway. The manifest
/// read is one query and the rebuild is O(n) over 667 sections, which is tens of
/// microseconds — cheaper than being wrong.
fn rebuild_geometry(store: &Store, geometry: &mut Geometry, document_id: &str) -> Result<()> {
    let manifest = store.manifest(document_id)?;
    let fresh = Geometry::from_manifest(&manifest);
    let measured = std::mem::replace(geometry, fresh);
    // Measured heights survive a rebuild by position. Positions are re-derived from
    // the new ordering, so this is only correct for sections at or below the change
    // point — but it is *closer* than discarding them all, and the frontend re-measures
    // anything it can see within a frame anyway.
    for (i, s) in measured.sections().iter().enumerate() {
        if s.measured && i < geometry.len() {
            geometry.update_measured_height(i, s.height);
        }
    }
    Ok(())
}

/// Fetch one section's compressed content.
///
/// # Why this is not just a bigger boot payload
///
/// The boot payload carries content for the first `BOOT_VISIBLE_SECTIONS` and the
/// manifest for everything. That is the right split for a first frame — the manifest is
/// ~100 bytes per section, so a 2000-page document is ~50KB and the scrollbar is correct
/// before anything is mounted — but it means sections past the window have a manifest row
/// and no content.
///
/// Without this command, such a section can only be left alone. It cannot be mounted
/// empty, because the first keystroke in an empty editor would overwrite the stored
/// content; and it cannot be fetched, so the document is scrollable but not editable past
/// its end. That is the gap this closes.
///
/// # Returns stored bytes, not a re-encode
///
/// `section_bytes` returns the blob verbatim. Re-encoding would compress the same bytes
/// twice over, and would mean the content the renderer receives depends on the zstd
/// version it was fetched with rather than on what is in the store.
pub fn get_section(store: &Store, document_id: Option<&str>, section_id: &str) -> Result<SectionContent> {
    let _ = document_id;
    // The store's `section_bytes` is the only thing consulted, and it errors for an
    // unknown section. That check is the point: a fetch for a section that does not
    // exist must be an error the frontend can distinguish from "the section has no
    // content", because the two need opposite handling.
    Ok(SectionContent { id: section_id.to_string(), content_zstd: store.section_bytes(section_id)? })
}

/// Append one edit to the write-ahead log.
///
/// # What is durable, and when
///
/// The row is committed to SQLite when this returns, so an edit survives a crash
/// immediately. It is *not* folded into the section blob until a flush, which
/// `get_document_boot` does on every open. That is the whole design: the keystroke path
/// appends a compressed blob and returns, and the expensive rewrite of the section
/// happens on a debounce.
///
/// So "saved" after this call means "in the recovery buffer", not "in the section row".
/// That distinction is why `flush` runs at boot, and why the frontend's unmount path
/// flushes rather than merely logging.
///
/// # Why the counts come back
///
/// Three of the four are derived here by `analyze`, from the bytes just written, and the
/// fourth is echoed. See [`CommitResponse`]. The short version: a caller that supplied a
/// `block_count` and did not get the authoritative one back would keep using its own, and
/// a section whose stored height estimate describes different bytes than it contains is
/// the defect `save_section`'s own comment says it prevents.
pub fn commit_section_edit(
    store: &Store,
    document_id: &str,
    section_id: &str,
    json: &Value,
    mark_count: u32,
) -> Result<CommitResponse> {
    let analyzed = analyze(json);

    // `plain_text` goes into the WAL row so `flush` can write it without decompressing.
    // It is derived here rather than sent from the frontend for the same reason the
    // counts are: one definition, in Rust.
    let metrics = SectionMetrics::new(analyzed.word_count, mark_count, analyzed.char_count);
    store.log_edit(document_id, section_id, json, metrics, &analyzed.text)?;

    let (wal_row_id, pending) = store.latest_wal_row(section_id, document_id)?;

    Ok(CommitResponse {
        section_id: section_id.to_string(),
        wal_row_id,
        word_count: analyzed.word_count,
        char_count: analyzed.char_count,
        block_count: analyzed.block_count,
        mark_count,
        pending,
    })
}

/// How many hits to retrieve before applying the caller's limit.
///
/// The `total` in [`SearchResponse`] has to survive the `LIMIT`, and FTS5 will not count
/// matches it was never asked for. So the query is run once at this ceiling and both the
/// page and the number come from it. 200 sections is comfortably more than any UI shows
/// and still one indexed walk on a 1,300-section document.
pub const SEARCH_HIT_LIMIT: usize = 200;

/// Search one document's plain text and return section-level hits.
pub fn search_document(
    store: &Store,
    document_id: &str,
    query: &str,
    limit: usize,
) -> Result<SearchResponse> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        // Not an error and not an empty result: it is a query the user has not finished
        // typing. `Store::search` would happily run it and match every section, so the
        // guard belongs here rather than in every caller.
        return Ok(SearchResponse { hits: Vec::new(), total: 0 });
    }

    let found = store.search(document_id, trimmed, SEARCH_HIT_LIMIT)?;
    let total = found.len();
    let hits = found
        .into_iter()
        .take(limit)
        .map(|h| SearchHit {
            section_id: h.section_id,
            snippet: h.snippet,
            score: h.score,
        })
        .collect();

    Ok(SearchResponse { hits, total })
}

/// Fold the write-ahead log for a document.
///
/// Exposed because the frontend's unmount path needs "saved" to mean "in the section
/// row" before it destroys an editor, and `commit_section_edit` only guarantees the log.
/// Calling this from the unmount handler is what makes the no-data-loss property true
/// rather than merely likely.
pub fn flush_document(store: &Store, document_id: &str) -> Result<u32> {
    Ok(store.flush(document_id)? as u32)
}

/// What a clean shutdown managed to do.
///
/// Returned rather than logged-only so the caller can decide whether the exit was clean,
/// and so a test can assert on it without capturing stderr. `wal_bytes` and
/// `pending_rows` being zero is the whole claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct ShutdownReport {
    /// Section snapshots folded from the logical log into their rows.
    pub flushed: u32,
    /// Documents that had pending rows, which can exceed one.
    pub documents: u32,
    /// Logical log rows left. Zero on a clean exit.
    pub pending_rows: u32,
    /// Bytes left in SQLite's own `-wal` sidecar. Zero on a clean exit.
    ///
    /// Reported rather than asserted inside this function: a store that is not
    /// file-backed legitimately has no sidecar, and a test needs to see the number.
    pub wal_bytes: i64,
    /// Asset rows reclaimed because no section referenced them any more.
    ///
    /// # Why this is in the shutdown report
    ///
    /// Because a garbage collector nobody can observe is one nobody believes ran. The
    /// count is zero for almost every shutdown, so "zero" means nothing on its own — but a
    /// user who deletes a 40MB figure and closes the document can see the number move, and
    /// a regression that stopped the sweep is visible rather than silent.
    ///
    /// Deliberately *not* part of `is_clean`. Leaving unreclaimed bytes behind is not a
    /// recovery problem — the bytes are still readable, just unused — so a sweep that
    /// failed must not make the exit look broken.
    pub assets_deleted: u32,
    /// Bytes those rows occupied. What the file gives back on the next `VACUUM`.
    pub assets_bytes: i64,
}

impl ShutdownReport {
    /// Whether the shutdown left nothing to recover.
    ///
    /// Both halves, and both are necessary: `pending_rows == 0` with a multi-megabyte
    /// `-wal` file means the data is durable but the file still looks stale to anything
    /// reading only the database; a truncated `-wal` with rows pending means the
    /// checkpointed the wrong log.
    pub fn is_clean(&self) -> bool {
        self.pending_rows == 0 && self.wal_bytes == 0
    }
}

/// Reclaim space in an open document: sweep orphaned assets, return free pages, truncate
/// the journal.
///
/// # The pipeline, and why that order
///
/// 1. **Sweep orphaned assets.** Delete rows no section references. This is the only step
///    that removes *content*, and it is first because the other two only rearrange pages.
/// 2. **`PRAGMA incremental_vacuum`.** Hand the pages step 1 freed back to the filesystem.
/// 3. **`PRAGMA wal_checkpoint(TRUNCATE)`.** Fold SQLite's own `-wal` sidecar away, so the
///    file on disk is self-contained.
///
/// Steps 2 and 3 are in that order because they are two different journals. `incremental_vacuum`
/// writes its truncation *through* the `-wal`, so truncating the journal afterwards would be
/// truncating something that has since changed; and a checkpoint without the vacuum leaves the
/// freed pages on the free list, where the next write reuses them and the file never shrinks.
///
/// # What "freed" means, precisely
///
/// This is the half of the story that is easy to get wrong, and the reason the wording above
/// says *return* rather than *shrink*.
///
/// `incremental_vacuum` only truncates pages at the **end** of the database file. A blob
/// freed from the middle is moved to the free list, where SQLite will reuse it for the next
/// write — but the file does not get smaller, and no amount of vacuuming fixes it. Only a
/// full `VACUUM` (which [`Store::backup_to`] performs via `VACUUM INTO`) rewrites the file
/// and reclaims interior space.
///
/// Measured on a 64 KiB image deleted from the middle of a file: 16 pages freed to the
/// freelist, `incremental_vacuum(16)` returned exactly one of them. So `pages_freed` below is
/// an honest count of what went back to the filesystem, not an estimate of what the user
/// might see in a file manager.
///
/// # Why it is safe to run on an open document
///
/// It takes the store lock like every other command, so it cannot interleave with a commit.
/// The sweep reads reachability from the `sections` rows, so it must run *after* the log is
/// folded — an image deleted a moment ago is still referenced in the WAL, and sweeping first
/// would delete a figure the next flush would bring back. [`Self::optimize_document`] calls
/// `flush_all` for exactly that reason.
pub fn optimize_document(store: &Store, document_id: &str) -> Result<OptimizeReport> {
    // Fold first. See the module note: reachability is read from `sections`, and the
    // deletion that made an asset an orphan may still be sitting in the log.
    let flushed = store.flush_all()? as u32;

    let before: i64 = store
        .conn()
        .query_row("PRAGMA page_count", [], |r| r.get(0))
        .unwrap_or(0);

    let sweep = holonomy_core::asset_gc::sweep_orphaned_assets(store, document_id)?;
    let assets_freed = sweep.bytes_freed;

    let freelist_before: i64 = store
        .conn()
        .query_row("PRAGMA freelist_count", [], |r| r.get(0))
        .unwrap_or(0);

    // `PRAGMA incremental_vacuum` with no argument releases exactly one page, so it is
    // driven in a loop bounded by what is actually free. Bounded because a pathological
    // freelist would otherwise turn a menu click into a long-running operation; the cap
    // is generous enough to reclaim a document's worth of deleted images in one go.
    let mut reclaimed = 0i64;
    for _ in 0..MAX_INCREMENTAL_VACUUM_STEPS {
        let free: i64 = store
            .conn()
            .query_row("PRAGMA freelist_count", [], |r| r.get(0))
            .unwrap_or(0);
        if free == 0 {
            break;
        }
        // `incremental_vacuum(N)` releases *up to* N trailing pages, which is one round trip
        // instead of N.
        if let Err(e) = store.conn().execute_batch(&format!("PRAGMA incremental_vacuum({free})")) {
            // Not fatal: the rows are gone and the pages are reusable. Report the shortfall
            // rather than failing an operation the user asked for to reclaim space.
            eprintln!("[holonomy] incremental vacuum stopped early: {e}");
            break;
        }
        reclaimed += 1;
        if reclaimed >= freelist_before {
            break;
        }
    }

    let after: i64 = store
        .conn()
        .query_row("PRAGMA page_count", [], |r| r.get(0))
        .unwrap_or(before);

    // Last, and for the reason in the module note.
    let wal_bytes = store.checkpoint_truncate()?;

    Ok(OptimizeReport {
        flushed,
        assets_deleted: sweep.deleted,
        assets_freed,
        pages_before: before,
        pages_after: after,
        pages_reclaimed: (before - after).max(0),
        wal_bytes,
    })
}

/// What [`optimize_document`] reclaimed.
///
/// Every field is measured rather than predicted, because the one a user can check by hand
/// is the file size in a file manager and a report that disagreed with it would be worse
/// than no report.
///
/// # Why the counts are `number` and not `bigint`
///
/// `ts-rs` maps Rust's 64-bit integers to `bigint`, which would force every caller to
/// compare against a `BigInt` literal and would make arithmetic in the notice line a
/// `BigInt` operation. The values are page counts and byte totals, so `i64` on the wire
/// would be exact anyway — but the *type* is the friction, not the width. `#[ts(type =
/// "number")]` keeps the generated contract honest about the fact that these are ordinary
/// counts, the same annotation `DocumentSummary::updated_at` already uses for the same
/// reason.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, TS)]
#[ts(export, export_to = "../../../app/src/core/generated-bridge.ts")]
#[serde(rename_all = "camelCase")]
pub struct OptimizeReport {
    /// Log rows folded before the sweep, without which an image deleted moments ago would
    /// still look referenced.
    pub flushed: u32,
    /// Asset rows deleted.
    pub assets_deleted: u32,
    /// Bytes those rows occupied.
    #[ts(type = "number")]
    pub assets_freed: i64,
    /// Database size in pages, before.
    #[ts(type = "number")]
    pub pages_before: i64,
    /// Database size in pages, after.
    #[ts(type = "number")]
    pub pages_after: i64,
    /// `pages_before - pages_after`, clamped at zero.
    ///
    /// Distinct from `assets_freed` on purpose: asset bytes are *content* removed, while
    /// this is *space returned*. An image freed from the middle of the file is fully
    /// removed and returns none, and conflating the two would promise a shrink the file
    /// does not deliver. See [`optimize_document`].
    #[ts(type = "number")]
    pub pages_reclaimed: i64,
    /// Bytes left in the `-wal` sidecar. Zero after a successful run.
    #[ts(type = "number")]
    pub wal_bytes: i64,
}

impl OptimizeReport {
    /// Whether anything at all was reclaimed.
    ///
    /// For the menu item's own feedback: "nothing to do" is a legitimate answer to
    /// "Optimize Document" and should read as information rather than as a failure.
    pub fn is_empty(&self) -> bool {
        self.assets_deleted == 0 && self.pages_reclaimed == 0 && self.wal_bytes == 0
    }
}

/// How many `incremental_vacuum` rounds to run at most.
///
/// One round can only reclaim trailing pages, so a loop is needed; the bound stops a
/// pathological freelist from turning a menu click into an unbounded operation. 4096 pages
/// is 16MB of default-4KiB page — far more than any realistic single cleanup.
const MAX_INCREMENTAL_VACUUM_STEPS: i64 = 4096;

/// Fold everything and checkpoint, in that order.
///
/// # Why this is a function and not a closure in the window handler
///
/// Because it has to be testable without a window, and because the order is load-bearing
/// and easy to get wrong in a handler: checkpointing first would leave the logical rows
/// unfolded and then report a clean file, which is the worst of both.
///
/// # Why it does not prevent the close
///
/// The handler calls this and lets the window close either way. Blocking a quit on a
/// storage error would mean a user cannot close an app whose disk is full or whose
/// database is locked by another process — and the data is already in the logical log,
/// so the next open recovers it. Refusing to close would trade a recoverable situation
/// for an unrecoverable one.
pub fn graceful_shutdown(store: &Store) -> Result<ShutdownReport> {
    let documents = store.documents_with_pending_wal()?;
    let flushed = store.flush_all()? as u32;

    // Reclaim asset bytes no section references any more.
    //
    // After the fold, because reachability is read from the `sections` rows and an image
    // deleted a moment ago is still sitting in the WAL until `flush_all` writes it. Running
    // this first would read the pre-delete content, see the image as still referenced, and
    // keep the bytes for a session — which is the behaviour this exists to remove, reached
    // by adding the feature.
    //
    // Once per document, and it is a sweep per document rather than one sweep overall: the
    // reachability scan is file-wide either way, so a second document would rescan every
    // section to delete nothing. The count is reported so a sweep that ran is
    // distinguishable from one that found nothing.
    let mut assets_deleted = 0u32;
    let mut assets_bytes = 0i64;
    for doc in store.documents().unwrap_or_default() {
        match holonomy_core::asset_gc::sweep_orphaned_assets(store, &doc.id) {
            Ok(report) => {
                assets_deleted += report.deleted;
                assets_bytes += report.bytes_freed;
            }
            // A sweep that fails must not block the close. The rows are not lost, only
            // unreclaimed, and the next open will try again. Refusing to close over a
            // garbage-collection failure would trade a recoverable situation for an
            // unrecoverable one, which is the same rule the WAL follows above.
            Err(e) => eprintln!("[holonomy] asset sweep for {} failed: {e}", doc.id),
        }
    }

    // Counted *before* the checkpoint, because `flush_all` should have emptied the log
    // and this is the number that says whether it did. Reporting the post-checkpoint
    // count would be zero by definition and the assertion would be vacuous.
    let pending_rows = store
        .documents_with_pending_wal()?
        .into_iter()
        .map(|id| store.wal().row_count(&id).unwrap_or(0))
        .sum::<u64>() as u32;

    let wal_bytes = store.checkpoint_truncate()?;

    Ok(ShutdownReport {
        flushed,
        documents: documents.len() as u32,
        pending_rows,
        wal_bytes,
        assets_deleted,
        assets_bytes,
    })
}

/// Build a throwaway document of `sections` sections, each `paragraphs` paragraphs long.
///
/// # Why the content is generated rather than a fixed string
///
/// Because the height model is only exercised by content with a realistic paragraph
/// ratio. One repeated string produces sections whose measured and predicted heights
/// agree for reasons that have nothing to do with the geometry, so a bound that made
/// heights wrong would look right.
///
/// # The bound
///
/// 512 sections. The verification asks for 50. The cap exists so a buggy caller cannot
/// fill the user's database with a hundred thousand fixture sections — this runs against
/// a real store, not a test one, and "the verification wrote 40MB of prose" is a
/// failure mode worth ruling out rather than trusting.
///
/// # Why it flushes
///
/// So the document is fully in its section rows before the harness reads it. Without the
/// flush the content would sit in the WAL, `get_section` would serve the pre-edit row,
/// and a re-hydration check would compare a placeholder against a placeholder.
pub fn create_ephemeral_document(
    store: &Store,
    sections: u32,
    paragraphs: u32,
) -> Result<super::bridge::EphemeralDocument> {
    const MAX_SECTIONS: u32 = 512;
    if sections == 0 || sections > MAX_SECTIONS {
        return Err(Error::Other(anyhow::anyhow!(
            "an ephemeral document is 1..={MAX_SECTIONS} sections, asked for {sections}"
        )));
    }
    if paragraphs == 0 || paragraphs > 64 {
        return Err(Error::Other(anyhow::anyhow!(
            "an ephemeral section is 1..=64 paragraphs, asked for {paragraphs}"
        )));
    }

    let doc = store.create_document("Verification fixture")?;
    let mut section_ids = Vec::with_capacity(sections as usize);
    for s in 0..sections {
        let blocks: Vec<serde_json::Value> = (0..paragraphs)
            .map(|p| {
                let text = format!(
                    "Section {s} paragraph {p}. {}",
                    "lorem ipsum dolor sit amet consectetur adipiscing elit ".repeat(3)
                );
                json!({"type": "paragraph", "content": [{"type": "text", "text": text}]})
            })
            .collect();
        section_ids.push(store.add_section(
            &doc.id,
            &json!({"type": "doc", "content": blocks}),
        )?);
    }
    store.flush(&doc.id)?;

    Ok(super::bridge::EphemeralDocument {
        document_id: doc.id,
        sections: sections as usize,
        boot_visible: BOOT_VISIBLE_SECTIONS.min(sections as usize),
        section_ids,
    })
}

/// Build a throwaway document at soak scale: ~1,000,000 words across ~1300 sections.
///
/// # Why this is a second builder rather than a bigger `create_ephemeral_document`
///
/// Because the 512-section cap on [`create_ephemeral_document`] is a *safety valve*, not a
/// statement about what the harness needs: it exists so a buggy caller cannot fill the user's
/// real database with a hundred thousand fixture sections. A soak deliberately runs past it,
/// and a document that is two orders of magnitude larger than the safety valve allows should
/// not be reachable by raising the valve — the cap would stop being a cap. So there are two
/// builders, two Tauri commands, two caps, and one `delete_ephemeral_document` for both.
///
/// # The arithmetic, and where the density comes from
///
/// `tests/export.rs` builds a 667-section, 20-paragraph fixture and measures it at 1,334,000
/// words — 100 per paragraph, or ~2,000 per section. A million words at *that* density would be
/// about 500 sections, which is not the question: the soak is asked whether the bounds hold at
/// 1300 sections, so the word count has to come down to the paragraph count instead.
///
/// ```text
/// 1,000,000 words ÷ 1,300 sections = 769 words/section
///                  769 ÷ 13 paragraphs = 59.2 words/paragraph
/// ```
///
/// Thirteen paragraphs, not the twenty the export corpus uses: at 20 paragraphs of the same
/// size the same 1300 sections would be 2.6M words, and the document would no longer be the
/// size the claim is about.
///
/// `SOAK_WORDS_PER_PARAGRAPH` below is 60, and it is exact rather than approximate because the
/// paragraph is a fixed prefix plus a fixed number of repeats of the eight-word filler:
/// `Section {s} paragraph {p}.` is 4 whitespace-separated tokens and
/// `"lorem ipsum dolor sit amet consectetur adipiscing elit "` × 7 is 56, for 60 per paragraph.
/// So 1,300 × 13 × 60 = 1,014,000 words — 1.4% over the million the harness asserts, and the
/// harness measures that rather than trusting this comment.
pub fn create_soak_document(
    store: &Store,
    sections: u32,
    paragraphs: u32,
) -> Result<super::bridge::EphemeralDocument> {
    // The filler length is derived from the constant rather than written as a literal `repeat(7)`,
    // so the paragraph cannot quietly stop being the size the arithmetic above describes.
    const SOAK_WORDS_PER_PARAGRAPH: usize = 60;
    const FILLER_WORDS: usize = 8;
    const PREFIX_WORDS: usize = 4;
    // At the caps that is 4096 × 32 × 60 ≈ 7.9M words. The measured cost is 7.4 bytes per word of
    // zstd'd ProseMirror JSON plus the plain text `analyze` extracts, so ~60MB written into the
    // user's real database. That is what a mistake here costs, and it is why these numbers are
    // lower than they could be rather than higher.
    const MAX_SECTIONS: u32 = 4096;
    const MAX_PARAGRAPHS: u32 = 32;
    if sections == 0 || sections > MAX_SECTIONS {
        return Err(Error::Other(anyhow::anyhow!(
            "a soak document is 1..={MAX_SECTIONS} sections, asked for {sections}"
        )));
    }
    if paragraphs == 0 || paragraphs > MAX_PARAGRAPHS {
        return Err(Error::Other(anyhow::anyhow!(
            "a soak section is 1..={MAX_PARAGRAPHS} paragraphs, asked for {paragraphs}"
        )));
    }

    // A distinct title, so a soak document that outlived its harness is identifiable in the
    // document list rather than being indistinguishable from the LRU fixture's litter.
    let doc = store.create_document("Soak fixture")?;
    let filler = "lorem ipsum dolor sit amet consectetur adipiscing elit "
        .repeat((SOAK_WORDS_PER_PARAGRAPH - PREFIX_WORDS) / FILLER_WORDS);
    let mut section_ids = Vec::with_capacity(sections as usize);
    for s in 0..sections {
        let blocks: Vec<serde_json::Value> = (0..paragraphs)
            .map(|p| {
                let text = format!("Section {s} paragraph {p}. {filler}");
                json!({"type": "paragraph", "content": [{"type": "text", "text": text}]})
            })
            .collect();
        section_ids.push(store.add_section(
            &doc.id,
            &json!({"type": "doc", "content": blocks}),
        )?);
    }
    store.flush(&doc.id)?;

    Ok(super::bridge::EphemeralDocument {
        document_id: doc.id,
        sections: sections as usize,
        boot_visible: BOOT_VISIBLE_SECTIONS.min(sections as usize),
        section_ids,
    })
}

/// Delete a fixture document, and everything left hanging off it.
pub fn delete_ephemeral_document(store: &Store, document_id: &str) -> Result<bool> {
    Ok(store.delete_document(document_id)? > 0)
}

/// What the `holo-asset://` handler answers with.
///
/// A status rather than a `Result`, because the handler's job is to turn a URL into an
/// HTTP-shaped answer. Modelling it as `Result` would force a 404 to be an error, and the
/// two need different handling: a 404 is a normal outcome of the user opening a document
/// whose assets have not synced, and an error is a bug.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetResponse {
    Found { mime: String, bytes: Vec<u8> },
    NotFound,
    /// The URL was not an asset URL at all — a different scheme, or a path that is not a
    /// bare hash.
    NotAnAsset,
}

/// Resolve a `holo-asset://` URL against the store.
///
/// # Why this is a function and not a closure inside the protocol registration
///
/// Because a `register_asynchronous_uri_scheme_protocol` handler cannot be tested without
/// a `tauri::AppHandle` and a webview, and this is the part that can be wrong. Splitting
/// it means the URL grammar and the store lookup are covered by ordinary tests, and the
/// adapter is the four lines that turn a `AssetResponse` into a `tauri::http::Response`.
///
/// # The grammar, and why it is so strict
///
/// `holo-asset://<64 lowercase hex>`. Nothing else. The handler reads bytes out of a
/// database and hands them to a renderer that will execute whatever it is told to render,
/// so a URL it accepts is a URL that must not be able to address anything but one hash of
/// one asset.
///
/// Three specific rejections, each for a different reason:
///
/// - **A wrong host.** `holo-asset://<hash>/../..` is a path traversal with extra steps;
///   there is no directory to traverse into, so the whole path is refused rather than
///   normalised.
/// - **A path.** No asset needs one. Accepting `holo-asset://<hash>/variant` would be a
///   second addressing scheme with no implementation behind it.
/// - **Uppercase hex.** `sha256_hex` emits lowercase, so uppercase cannot match a stored
///   key. Rejecting it turns a typo into a 404 instead of a silently-missing asset, and
///   it means one hash has exactly one spelling.
///
/// The `url` crate would parse this correctly and permissively. Doing it by hand here is
/// deliberate: the grammar is four lines, the crate's defaults would be a second source of
/// rules, and a stricter-than-needed parser is not a liability when the only valid URL is
/// 64 hex characters.
pub fn resolve_asset_uri(store: &Store, uri: &str) -> Result<AssetResponse> {
    let rest = match uri.strip_prefix(&format!("{}://", holonomy_core::ASSET_SCHEME)) {
        Some(r) => r,
        None => return Ok(AssetResponse::NotAnAsset),
    };
    if rest.len() != 64 {
        return Ok(AssetResponse::NotAnAsset);
    }
    if !rest.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return Ok(AssetResponse::NotAnAsset);
    }
    match store.get_asset(rest)? {
        Some((mime, bytes)) => Ok(AssetResponse::Found { mime, bytes }),
        None => Ok(AssetResponse::NotFound),
    }
}

/// Build the URL an image node stores, from a hash.
///
/// The inverse of [`resolve_asset_uri`], and in the same module so the two cannot drift.
pub fn asset_uri(hash: &str) -> String {
    format!("{}://{hash}", holonomy_core::ASSET_SCHEME)
}
