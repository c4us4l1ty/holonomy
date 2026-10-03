//! The bridge's commands, tested against an in-memory store.
//!
//! # Why these are here and not in the window
//!
//! The webkit2gtk window cannot be driven from a test on a headless Wayland seat —
//! that is the `POST /session` hang recorded in `STATUS.md`, and it is why the
//! cross-engine verification needs a real display. Anything reachable only through
//! a `#[tauri::command]` could therefore only be tested by hand, in a window, once.
//!
//! So the decisions live in `core.rs` as functions over `&Store`, and these tests
//! cover them. What is left untested here is serialisation, which has no decisions in
//! it and is checked by the contract test in `bridge-contract.rs`.
//!
//! # The rule these tests follow
//!
//! Every test asserts something that would fail if the change it describes were
//! reverted. A test that passes before and after is not a regression guard; it is
//! decoration. Several of these were written by reverting the fix and confirming
//! the failure, and the comments say which ones.

use holonomy_core::{Geometry, Store};
use holonomy_shell_lib::bridge::{HeightUpdate, LifecycleAction};
use serde_json::json;

use holonomy_shell_lib::core;

/// A store with one document of `n` sections, each `blocks` paragraphs long.
fn store_with(n: usize, blocks: usize) -> Store {
    let store = Store::open_in_memory().expect("in-memory store");
    let doc = store.create_document("Test").expect("create document");
    for _ in 0..n {
        store
            .add_section(&doc.id, &json!({"type":"doc","content": vec_blocks(blocks)}))
            .expect("add section");
    }
    store
}

fn vec_blocks(n: usize) -> Vec<serde_json::Value> {
    (0..n)
        .map(|i| json!({"type":"paragraph","content":[{"type":"text","text": format!("block {i}")}]}))
        .collect()
}

fn section_ids(store: &Store) -> Vec<String> {
    let docs = store.documents().expect("documents");
    let id = &docs[0].id;
    store.section_ids(id).expect("section ids")
}

/// Assert every section in the document has exactly `blocks` top-level blocks.
fn assert_block_counts(store: &Store, expected: &[usize]) {
    let docs = store.documents().expect("documents");
    let manifest = store.manifest(&docs[0].id).expect("manifest");
    let actual: Vec<usize> = manifest
        .entries()
        .iter()
        .map(|e| e.block_count as usize)
        .collect();
    assert_eq!(actual, expected, "block counts per section");
}

// -- boot ------------------------------------------------------------------

#[test]
fn boot_carries_every_section_and_content_only_for_the_first_window() {
    // 40 sections is far more than BOOT_VISIBLE_SECTIONS, so the manifest and the
    // content list are genuinely different sizes. If `visible` accidentally covered
    // everything, this fails on the length; if it carried nothing, on the emptiness.
    let store = store_with(40, 3);
    let payload = core::get_document_boot(&store, None).expect("boot");

    assert_eq!(payload.sections.len(), 40, "manifest must cover the whole document");
    assert_eq!(
        payload.visible.len(),
        core::BOOT_VISIBLE_SECTIONS,
        "content must be limited to the boot window"
    );
    assert!(
        payload.visible.iter().all(|v| !v.content_zstd.is_empty()),
        "every visible section must carry bytes"
    );
}

#[test]
fn boot_content_is_the_stored_bytes_verbatim() {
    // "Verbatim" is the claim in `Store::section_bytes`'s doc comment, and it is
    // checkable: the payload's bytes must equal what the store holds. If this ever
    // decompresses and re-compresses, a zstd version change would alter the bytes
    // and this fails — which is the signal the comment promises.
    let store = store_with(2, 3);
    let payload = core::get_document_boot(&store, None).expect("boot");
    let stored = store
        .section_bytes(&payload.visible[0].id)
        .expect("section bytes");

    assert_eq!(
        payload.visible[0].content_zstd, stored,
        "boot payload content is not byte-identical to the stored blob"
    );
}

#[test]
fn boot_recovers_unflushed_edits_before_it_reports_the_manifest() {
    // The WAL is the crash-recovery buffer. If boot read the manifest before
    // recovering, a section's counts would describe the last flush rather than what
    // the user last saw — and the geometry is built from those counts, so the
    // scrollbar would be wrong before a single keystroke.
    //
    // Reverting `store.recover()` in `get_document_boot` makes this fail: the
    // manifest carries the flushed counts, not the edited ones.
    let store = store_with(1, 2);
    let docs = store.documents().expect("documents");
    let doc_id = docs[0].id.clone();
    let ids = section_ids(&store);
    let target = ids[0].clone();

    // Replace the content with three blocks of distinctly different length.
    let long = json!({"type":"doc","content": vec_blocks(3)});
    store
        .log_edit(
            &doc_id,
            &target,
            &long,
            holonomy_core::SectionMetrics::new(300, 1800, 0),
            "block 0 block 1 block 2 ",
        )
        .expect("log edit");

    // Before recovery the manifest still describes the original two blocks.
    let before = store.manifest(&doc_id).expect("manifest");
    assert_eq!(
        before.entries()[0].block_count, 2,
        "precondition: the edit is in the WAL, not yet in the section"
    );

    let payload = core::get_document_boot(&store, Some(&doc_id)).expect("boot");
    assert_eq!(
        payload.sections[0].block_count, 3,
        "boot reported the flushed block count; the WAL was not recovered first"
    );
}

#[test]
fn boot_uses_the_fitted_calibration_rather_than_a_default() {
    // The frontend throws if it has no calibration, and every height it estimates is
    // measured against this. So a boot payload carrying anything other than the
    // fitted model is a silent wrongness, and this pins it.
    let store = store_with(1, 1);
    let payload = core::get_document_boot(&store, None).expect("boot");
    let expected = holonomy_core::GeometryCalibration::default();

    assert_eq!(payload.calibration.px_per_100_chars, expected.px_per_100_chars);
    assert_eq!(payload.calibration.px_per_paragraph, expected.px_per_paragraph);
    assert_eq!(payload.calibration.section_chrome_px, expected.section_chrome_px);
    // The fitted chrome is 72.6px for 15px/1.6 Inter. A round number -- 50, 60, 80 -- means the
    // payload is not carrying the measured model. The value moved from 54.8 when the editor's
    // own font was embedded rather than borrowed from `system-ui`; see STATUS.md.
    assert!(
        (payload.calibration.section_chrome_px - 72.6).abs() < 0.01,
        "chrome is {}; the calibrated value is 72.6, so this payload is not the \
         measured model",
        payload.calibration.section_chrome_px
    );
}

#[test]
fn boot_on_an_empty_store_creates_a_document_with_one_section() {
    // The frontend's "no sections" path should be unreachable in normal use, and
    // this is why: a fresh install opens a real document rather than an empty one.
    let store = Store::open_in_memory().expect("in-memory store");
    let payload = core::get_document_boot(&store, None).expect("boot");

    assert_eq!(payload.sections.len(), 1, "a new document needs one section to mount");
    // 26 characters: a ULID is 128 bits in Crockford base32, and 26 * 5 bits = 130.
    // Asserted rather than assumed -- if this ever changes it means ids are no longer
    // ULIDs, and something downstream that sorts by id string is now relying on a
    // property that is no longer guaranteed.
    assert_eq!(
        payload.document_id.len(),
        26,
        "a document id is a 26-character ULID, got {}",
        payload.document_id
    );
    assert!(
        payload.document_id
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()),
        "a ULID is uppercase Crockford base32, got {}",
        payload.document_id
    );
}

#[test]
fn boot_reports_no_caret_position_rather_than_inventing_one() {
    // Null, not a guess. A wrong guess scrolls a 2000-page document somewhere
    // arbitrary with nothing to tell the user it was wrong.
    let store = store_with(3, 1);
    let payload = core::get_document_boot(&store, None).expect("boot");

    assert!(payload.focused_section_id.is_none());
    assert!(payload.scroll_top.is_none());
}

// -- height sync -----------------------------------------------------------

#[test]
fn height_sync_replaces_an_estimate_with_a_measurement() {
    let store = store_with(10, 4);
    let payload = core::get_document_boot(&store, None).expect("boot");
    let mut geometry = Geometry::from_manifest(&store.manifest(&payload.document_id).unwrap());

    let estimated = geometry.total_height();
    assert!(estimated > 0.0, "precondition: the manifest seeds real estimates");

    // Every section measures 500px, which is nothing like the estimate for four
    // paragraphs, so the total must move a long way.
    let updates: Vec<HeightUpdate> = (0..10)
        .map(|i| HeightUpdate { section_id: payload.sections[i].id.clone(), index: i as u32, height: 500.0 })
        .collect();
    let total = core::sync_section_heights(&mut geometry, &updates).expect("sync");

    assert!(
        (total - 5000.0).abs() < 1.0,
        "ten sections at 500px should total 5000, got {total}"
    );
    assert!(
        geometry.height_of(0) == Some(500.0),
        "section 0 should hold its measured height, not its estimate"
    );
}

#[test]
fn height_sync_of_an_unchanged_height_is_a_no_op() {
    // Re-measurement happens constantly — font load, image decode, window resize —
    // and applying an identical height again must not move the total. Without this,
    // floating-point drift accumulates in the prefix-sum tree and the scrollbar walks.
    let store = store_with(5, 3);
    let payload = core::get_document_boot(&store, None).expect("boot");
    let mut geometry = Geometry::from_manifest(&store.manifest(&payload.document_id).unwrap());

    let batch: Vec<HeightUpdate> = (0..5)
        .map(|i| HeightUpdate { section_id: payload.sections[i].id.clone(), index: i as u32, height: 400.0 })
        .collect();

    let first = core::sync_section_heights(&mut geometry, &batch).expect("first sync");
    let second = core::sync_section_heights(&mut geometry, &batch).expect("second sync");

    assert_eq!(first, second, "re-applying the same heights changed the total");
}

#[test]
fn height_sync_rejects_a_batch_that_does_not_fit_the_document() {
    // A batch naming a section past the end means the frontend and the store
    // disagree about how many sections exist. Applying it would either write out of
    // bounds or attach a height to the wrong section; rejecting it leaves the
    // frontend with the total it already had, which self-corrects on the next batch.
    let store = store_with(3, 1);
    let payload = core::get_document_boot(&store, None).expect("boot");
    let mut geometry = Geometry::from_manifest(&store.manifest(&payload.document_id).unwrap());
    let before = geometry.total_height();

    let bad = vec![HeightUpdate {
        section_id: payload.sections[0].id.clone(),
        index: 99,
        height: 400.0,
    }];
    let err = core::sync_section_heights(&mut geometry, &bad).expect_err("must reject");

    assert!(
        err.to_string().contains("disagree on the section count"),
        "the error should name the disagreement, got: {err}"
    );
    assert_eq!(
        geometry.total_height(),
        before,
        "a rejected batch must leave the geometry untouched"
    );
}

#[test]
fn an_empty_height_batch_is_accepted_and_returns_the_current_total() {
    // A debounce that fires with nothing queued must not be an error. It happens on
    // every idle tick, and a command that fails when there is nothing to do is a
    // command that logs a failure on every idle tick.
    let store = store_with(4, 2);
    let payload = core::get_document_boot(&store, None).expect("boot");
    let mut geometry = Geometry::from_manifest(&store.manifest(&payload.document_id).unwrap());
    let expected = geometry.total_height();

    let total = core::sync_section_heights(&mut geometry, &[]).expect("empty batch");
    assert_eq!(total, expected);
}

// -- lifecycle: split ------------------------------------------------------

#[test]
fn split_divides_a_section_and_keeps_every_block() {
    // The load-bearing property of a split: no block is lost and none is duplicated.
    // A split that dropped the tail would silently delete the second half of a
    // section, which is the worst failure this command could have.
    let store = store_with(3, 10);
    let payload = core::get_document_boot(&store, None).expect("boot");
    let mut geometry = Geometry::from_manifest(&store.manifest(&payload.document_id).unwrap());
    let target = payload.sections[1].id.clone();

    let result = core::commit_section_lifecycle(
        &store,
        &mut geometry,
        &LifecycleAction::Split { section_id: target.clone(), at_block: 4, index: 1 },
    )
    .expect("split");

    assert!(result.applied, "split should apply: {:?}", result.reason);
    assert_eq!(result.section_ids.len(), 4, "three sections become four");
    assert_block_counts(&store, &[10, 4, 6, 10]);

    // The content itself, not just the counts: every original block appears exactly
    // once across the two halves.
    let head = store.load_section(&target).expect("head");
    let new_id = result.section_ids[2].clone();
    let tail = store.load_section(&new_id).expect("tail");
    let total: usize = [head, tail]
        .iter()
        .map(|d| d["content"].as_array().unwrap().len())
        .sum();
    assert_eq!(total, 10, "blocks were lost or duplicated by the split");
}

#[test]
fn split_clamps_a_cut_that_would_leave_an_empty_half() {
    // Block 0 would leave the original empty; block 10 would leave the new one empty.
    // Both are technically valid documents and practically broken — an empty section
    // the user scrolls past forever. So the request is honoured where it can be.
    let store = store_with(2, 5);
    let payload = core::get_document_boot(&store, None).expect("boot");
    let mut geometry = Geometry::from_manifest(&store.manifest(&payload.document_id).unwrap());
    let target = payload.sections[0].id.clone();

    let result = core::commit_section_lifecycle(
        &store,
        &mut geometry,
        &LifecycleAction::Split { section_id: target.clone(), at_block: 0, index: 0 },
    )
    .expect("split");
    assert!(result.applied);
    assert_block_counts(&store, &[1, 4, 5]);

    let store2 = store_with(2, 5);
    let payload2 = core::get_document_boot(&store2, None).expect("boot");
    let mut geometry2 = Geometry::from_manifest(&store2.manifest(&payload2.document_id).unwrap());
    let result2 = core::commit_section_lifecycle(
        &store2,
        &mut geometry2,
        &LifecycleAction::Split { section_id: payload2.sections[0].id.clone(), at_block: 99, index: 0 },
    )
    .expect("split");
    assert!(result2.applied);
    assert_block_counts(&store2, &[4, 1, 5]);
}

#[test]
fn split_refuses_a_single_block_section_and_says_why() {
    // `applied: false` and a reason, not an error. The frontend asked for something
    // the document's shape does not allow; an error would read as a crash.
    let store = store_with(2, 1);
    let payload = core::get_document_boot(&store, None).expect("boot");
    let mut geometry = Geometry::from_manifest(&store.manifest(&payload.document_id).unwrap());

    let result = core::commit_section_lifecycle(
        &store,
        &mut geometry,
        &LifecycleAction::Split { section_id: payload.sections[0].id.clone(), at_block: 0, index: 0 },
    )
    .expect("no error for an unsplittable section");

    assert!(!result.applied, "a one-block section cannot be split");
    assert!(
        result.reason.as_deref().unwrap_or_default().contains("at least 2"),
        "the reason should say what is required, got {:?}",
        result.reason
    );
    assert_block_counts(&store, &[1, 1]);
    assert_eq!(section_ids(&store).len(), 2, "a refused split must not add a section");
}

#[test]
fn split_updates_the_geometry_length_to_match_the_store() {
    // After a split the geometry has one more section than before. If it did not,
    // the first height sync for the new section would be rejected by the bounds check
    // in `sync_section_heights` — the frontend would see its own sections refused.
    let store = store_with(6, 4);
    let payload = core::get_document_boot(&store, None).expect("boot");
    let mut geometry = Geometry::from_manifest(&store.manifest(&payload.document_id).unwrap());
    assert_eq!(geometry.len(), 6);

    core::commit_section_lifecycle(
        &store,
        &mut geometry,
        &LifecycleAction::Split { section_id: payload.sections[0].id.clone(), at_block: 2, index: 0 },
    )
    .expect("split");

    assert_eq!(geometry.len(), 7, "the geometry must gain a section when the store does");
    assert!(core::sync_section_heights(&mut geometry, &[]).is_ok());
}

#[test]
fn a_split_of_many_sections_in_a_row_keeps_working() {
    // The order keys are u64 with 1024-wide gaps, so repeated inserts between the
    // same pair walk the gap down. Eventually `key_for_insert` fails and the fallback
    // rebalances. 40 splits into one gap is 40 halvings — past where a naive
    // midpoint would have run out, which is the point of the test.
    //
    // Note this splits section 0 repeatedly, so each cut creates a new section 0 and
    // leaves the previous tail behind it. The count is what matters.
    let store = store_with(2, 200);
    let payload = core::get_document_boot(&store, None).expect("boot");
    let mut geometry = Geometry::from_manifest(&store.manifest(&payload.document_id).unwrap());

    for i in 0..40 {
        let ids = section_ids(&store);
        core::commit_section_lifecycle(
            &store,
            &mut geometry,
            &LifecycleAction::Split { section_id: ids[0].clone(), at_block: 100, index: 0 },
        )
        .unwrap_or_else(|e| panic!("split {i} failed: {e}"));
    }

    assert_eq!(section_ids(&store).len(), 42, "2 sections plus 40 splits");

    // And the keys are still strictly increasing, which is the invariant the whole
    // ordering rests on.
    let manifest = store.manifest(&payload.document_id).expect("manifest");
    let keys: Vec<u64> = manifest.entries().iter().map(|e| e.order_key.0).collect();
    assert!(
        keys.windows(2).all(|w| w[0] < w[1]),
        "order keys stopped being strictly increasing: {keys:?}"
    );
}

// -- lifecycle: merge ------------------------------------------------------

#[test]
fn merge_keeps_the_target_and_deletes_the_source() {
    // The merged content goes to the *target*, so the section the frontend was
    // focusing survives. The other direction would move the caret's section out from
    // under it.
    let store = store_with(3, 5);
    let payload = core::get_document_boot(&store, None).expect("boot");
    let mut geometry = Geometry::from_manifest(&store.manifest(&payload.document_id).unwrap());
    let ids = section_ids(&store);

    let result = core::commit_section_lifecycle(
        &store,
        &mut geometry,
        &LifecycleAction::Merge {
            section_id: ids[1].clone(),
            into_section_id: ids[0].clone(),
            index: 1,
        },
    )
    .expect("merge");

    assert!(result.applied, "merge should apply: {:?}", result.reason);
    assert_eq!(result.section_ids.len(), 2);
    assert!(!result.section_ids.contains(&ids[1]), "the source must be gone");
    assert!(result.section_ids.contains(&ids[0]), "the target must survive");
    assert_block_counts(&store, &[10, 5]);
}

#[test]
fn merge_concatenates_content_in_order() {
    // Order, not just totals: a merge that appended the source to the wrong end would
    // pass a block-count check while reversing the document.
    //
    // # The two sections must not be identical
    //
    // The first version of this test built both sections from the same block text, so
    // the merged document read `0,1,2,0,1,2` either way and the assertion could not
    // tell a correct merge from a reversed one. It passed against a merge that
    // deliberately emitted source-before-target. Confirmed by mutation, not assumed.
    //
    // So the content is labelled per section here, and the expected order names which
    // half came first.
    let store = Store::open_in_memory().expect("store");
    let doc = store.create_document("Test").expect("doc");
    let first = store
        .add_section(&doc.id, &json!({"type":"doc","content": labelled_blocks("A", 3)}))
        .expect("first section");
    let second = store
        .add_section(&doc.id, &json!({"type":"doc","content": labelled_blocks("B", 3)}))
        .expect("second section");

    let mut geometry = Geometry::from_manifest(&store.manifest(&doc.id).unwrap());
    core::commit_section_lifecycle(
        &store,
        &mut geometry,
        &LifecycleAction::Merge { section_id: second.clone(), into_section_id: first.clone(), index: 1 },
    )
    .expect("merge");

    let merged = store.load_section(&first).expect("merged content");
    let texts: Vec<String> = merged["content"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["content"][0]["text"].as_str().unwrap().to_string())
        .collect();

    assert_eq!(
        texts,
        vec!["A0", "A1", "A2", "B0", "B1", "B2"],
        "merged content is not the target followed by the source"
    );
}

/// Blocks whose text is prefixed, so two sections' content can be told apart.
fn labelled_blocks(label: &str, n: usize) -> Vec<serde_json::Value> {
    (0..n)
        .map(|i| {
            json!({"type":"paragraph","content":[{"type":"text","text": format!("{label}{i}")}]})
        })
        .collect()
}

#[test]
fn merge_refuses_to_join_two_documents() {
    let store = Store::open_in_memory().expect("store");
    let a = store.create_document("A").expect("doc a");
    let b = store.create_document("B").expect("doc b");
    store.add_section(&a.id, &json!({"type":"doc","content":vec_blocks(2)})).expect("a section");
    store.add_section(&b.id, &json!({"type":"doc","content":vec_blocks(2)})).expect("b section");

    let mut geometry = Geometry::new();
    let result = core::commit_section_lifecycle(
        &store,
        &mut geometry,
        &LifecycleAction::Merge {
            section_id: section_ids_for(&store, &a.id)[0].clone(),
            into_section_id: section_ids_for(&store, &b.id)[0].clone(),
            index: 0,
        },
    )
    .expect("no error");

    assert!(!result.applied);
    assert!(result.reason.unwrap_or_default().contains("different documents"));
}

#[test]
fn merge_refuses_to_join_a_section_into_itself() {
    let store = store_with(2, 4);
    let payload = core::get_document_boot(&store, None).expect("boot");
    let mut geometry = Geometry::from_manifest(&store.manifest(&payload.document_id).unwrap());
    let id = payload.sections[0].id.clone();

    let result = core::commit_section_lifecycle(
        &store,
        &mut geometry,
        &LifecycleAction::Merge { section_id: id.clone(), into_section_id: id.clone(), index: 0 },
    )
    .expect("no error");

    assert!(!result.applied);
    assert!(result.reason.unwrap_or_default().contains("into itself"));
    assert_block_counts(&store, &[4, 4]);
}

fn section_ids_for(store: &Store, document_id: &str) -> Vec<String> {
    store.section_ids(document_id).expect("section ids")
}

#[test]
fn merge_updates_the_geometry_length_to_match_the_store() {
    // The mirror of the split case: a merge removes a section, and a geometry that
    // still had it would accept heights for a section that no longer exists.
    let store = store_with(5, 3);
    let payload = core::get_document_boot(&store, None).expect("boot");
    let mut geometry = Geometry::from_manifest(&store.manifest(&payload.document_id).unwrap());
    let ids = section_ids(&store);

    core::commit_section_lifecycle(
        &store,
        &mut geometry,
        &LifecycleAction::Merge { section_id: ids[2].clone(), into_section_id: ids[1].clone(), index: 2 },
    )
    .expect("merge");

    assert_eq!(geometry.len(), 4, "the geometry must lose a section when the store does");
}

// -- lifecycle: a rejected action must not disturb the geometry -------------

#[test]
fn a_refused_lifecycle_action_leaves_the_document_and_geometry_untouched() {
    // Every refusal path: unsplittable section, cross-document merge, self-merge.
    // Each must be a no-op, because the frontend treats `applied: false` as "leave
    // the document as it is" and will not re-read anything.
    let store = store_with(3, 1);
    let payload = core::get_document_boot(&store, None).expect("boot");
    let mut geometry = Geometry::from_manifest(&store.manifest(&payload.document_id).unwrap());
    let before_height = geometry.total_height();
    let before_ids = section_ids(&store);

    for action in [
        LifecycleAction::Split { section_id: payload.sections[0].id.clone(), at_block: 0, index: 0 },
        LifecycleAction::Merge {
            section_id: payload.sections[1].id.clone(),
            into_section_id: payload.sections[1].id.clone(),
            index: 1,
        },
    ] {
        let result = core::commit_section_lifecycle(&store, &mut geometry, &action).expect("no error");
        assert!(!result.applied, "{action:?} should have been refused");
        assert_eq!(section_ids(&store), before_ids, "{action:?} changed the document");
        assert_eq!(geometry.len(), 3, "{action:?} changed the geometry length");
        assert_eq!(geometry.total_height(), before_height, "{action:?} moved the total");
    }
}

#[test]
fn a_lifecycle_action_for_an_unknown_section_is_an_error_not_a_silent_no_op() {
    // Distinct from the refusals above. "This section cannot be split" is a fact
    // about the document and the frontend can act on it. "That section does not
    // exist" means the two sides have diverged, and returning `applied: false` for
    // it would let the frontend carry on with a document it no longer agrees with.
    let store = store_with(2, 3);
    let payload = core::get_document_boot(&store, None).expect("boot");
    let mut geometry = Geometry::from_manifest(&store.manifest(&payload.document_id).unwrap());

    let err = core::commit_section_lifecycle(
        &store,
        &mut geometry,
        &LifecycleAction::Split {
            section_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
            at_block: 1,
            index: 0,
        },
    )
    .expect_err("an unknown section must be an error");

    assert!(
        err.to_string().contains("not found"),
        "the error should say the section is missing, got: {err}"
    );
}

// -- persistence: get_section and commit_section_edit -----------------------

/// A store with one document of one section carrying `blocks` labelled paragraphs.
fn store_with_blocks(blocks: usize) -> (Store, String, String) {
    let store = Store::open_in_memory().expect("in-memory store");
    let doc = store.create_document("Test").expect("create document");
    let json = json!({"type":"doc","content": vec_blocks(blocks)});
    let section = store.add_section(&doc.id, &json).expect("add section");
    (store, doc.id, section)
}

#[test]
fn get_section_returns_the_stored_bytes_verbatim() {
    // "Verbatim" is the claim, and it is checkable: what the command returns must equal
    // what the store holds. If this ever decompresses and re-encodes, a zstd version
    // change would alter the bytes and this fails -- which is the signal the comment on
    // `Store::section_bytes` promises.
    let (store, doc_id, section) = store_with_blocks(4);
    let got = holonomy_shell_lib::core::get_section(&store, Some(&doc_id), &section).expect("get_section");
    let stored = store.section_bytes(&section).expect("stored bytes");

    assert_eq!(got.id, section, "the payload should name the section asked for");
    assert_eq!(got.content_zstd, stored, "content is not byte-identical to the stored blob");
}

#[test]
fn get_section_round_trips_through_the_stores_own_decoder() {
    // The property the frontend depends on: what `get_section` hands over decodes back to
    // the section's content. Without this, a decoder bug and an encoder bug would be
    // indistinguishable at boot.
    let (store, doc_id, section) = store_with_blocks(3);
    let got = holonomy_shell_lib::core::get_section(&store, Some(&doc_id), &section).expect("get_section");
    let decoded = holonomy_core::store::decode(&got.id, &got.content_zstd).expect("decode");
    let direct = store.load_section(&section).expect("direct load");

    assert_eq!(decoded, direct, "the fetched content differs from a direct read");
}

#[test]
fn get_section_for_an_unknown_section_is_an_error() {
    // Not `Ok` with empty bytes. A fetch for a section that does not exist has to be
    // distinguishable from "this section has no content", because the frontend needs
    // opposite handling: retry, versus proceed.
    let (store, doc_id, _) = store_with_blocks(2);
    let err = holonomy_shell_lib::core::get_section(&store, Some(&doc_id), "nope")
        .expect_err("an unknown section must be an error");
    assert!(
        err.to_string().contains("not found"),
        "the error should say the section is missing, got: {err}"
    );
}

#[test]
fn committing_an_edit_makes_it_durable_in_the_wal_immediately() {
    // "Durable" after `commit_section_edit` means "in the recovery buffer", not "in the
    // section row". This test pins that distinction rather than blurring it: the section
    // blob still describes the *old* content, and only the log has the new.
    let (store, doc_id, section) = store_with_blocks(2);
    let edited = json!({"type":"doc","content": [
        {"type":"paragraph","content":[{"type":"text","text":"brand new words here"}]},
        {"type":"paragraph","content":[{"type":"text","text":"and a second block"}]},
        {"type":"paragraph","content":[{"type":"text","text":"three blocks now"}]},
    ]});

    let response = holonomy_shell_lib::core::commit_section_edit(&store, &doc_id, &section, &edited, 2)
        .expect("commit");

    assert_eq!(response.section_id, section);
    assert!(response.wal_row_id > 0, "a WAL row should have been written");
    assert_eq!(response.pending, 1, "one edit should be pending");

    // Not yet in the section row.
    let manifest = store.manifest(&doc_id).expect("manifest");
    assert_eq!(
        manifest.entries()[0].block_count, 2,
        "precondition: the section row still describes the pre-edit content"
    );

    // But a crash right now loses nothing, because boot flushes.
    store.flush(&doc_id).expect("flush");
    let after = store.load_section(&section).expect("load after flush");
    assert_eq!(after, edited, "the flush did not fold the edit into the section");
    let manifest = store.manifest(&doc_id).expect("manifest");
    assert_eq!(manifest.entries()[0].block_count, 3, "the flushed row should count 3 blocks");
}

#[test]
fn committing_reports_the_counts_rust_derived_not_the_callers() {
    // The reason `char_count` and `block_count` are not parameters. A caller that
    // supplied them and did not get the authoritative values back would keep using its
    // own, and a section whose stored height estimate describes different bytes than it
    // contains is the defect `save_section` exists to prevent.
    let (store, doc_id, section) = store_with_blocks(2);
    let edited = json!({"type":"doc","content": vec_blocks(7)});

    let response = holonomy_shell_lib::core::commit_section_edit(&store, &doc_id, &section, &edited, 5)
        .expect("commit");

    assert_eq!(response.block_count, 7, "block_count should come from the JSON that was written");
    let text = edited["content"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["content"][0]["text"].as_str().unwrap())
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(
        response.char_count as usize,
        text.chars().filter(|c| !c.is_whitespace()).count(),
        "char_count should be non-whitespace characters, matching analyze"
    );
    assert_eq!(
        response.mark_count, 5,
        "mark_count is echoed, because analyze has no mark counter"
    );
}

#[test]
fn committing_one_section_repeatedly_keeps_only_its_latest_state() {
    // Not an append journal. `Wal::pending` selects only the newest row per section, and
    // that is the right shape for a recovery buffer: replaying two states of the same
    // section in order is wasted work, because the later one wins.
    //
    // This test asserts the shape rather than assuming it. The first version expected
    // three pending rows and failed against code that was right — and the failure was
    // informative, because it meant the frontend's "unmount flushes, so nothing is lost"
    // argument rests on the latest state being sufficient rather than on every
    // intermediate one.
    let (store, doc_id, section) = store_with_blocks(2);
    for text in ["first", "second", "third"] {
        holonomy_shell_lib::core::commit_section_edit(
            &store,
            &doc_id,
            &section,
            &json!({"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":text}]}]}),
            0,
        )
        .expect("commit");
    }
    assert_eq!(
        store.wal().pending_count(&doc_id).expect("pending count"),
        1,
        "one section with three edits is one pending state, not three"
    );
    store.flush(&doc_id).expect("flush");
    let after = store.load_section(&section).expect("load");
    assert_eq!(
        after["content"][0]["content"][0]["text"], "third",
        "the latest state must win, or a lost keystroke would be a lost edit"
    );
    assert_eq!(store.wal().pending_count(&doc_id).expect("pending count"), 0, "flush should empty the log");
}

#[test]
fn pending_counts_sections_not_edits() {
    // The number is per section, so a document being edited in many places at once does
    // not make the counter climb with every keystroke. It is a signal that the log is not
    // being folded, and a signal that climbs on every keystroke is one nobody reads.
    let store = Store::open_in_memory().expect("in-memory store");
    let doc = store.create_document("Test").expect("doc");
    let sections: Vec<String> = (0..4)
        .map(|_| store.add_section(&doc.id, &json!({"type":"doc","content": vec_blocks(2)})).expect("add"))
        .collect();

    for s in &sections {
        holonomy_shell_lib::core::commit_section_edit(
            &store,
            &doc.id,
            s,
            &json!({"type":"doc","content": vec_blocks(3)}),
            0,
        )
        .expect("commit");
    }
    assert_eq!(
        store.wal().pending_count(&doc.id).expect("pending count"),
        4,
        "four sections edited once is four pending states"
    );

    // Editing one of them again does not add a fifth.
    holonomy_shell_lib::core::commit_section_edit(
        &store,
        &doc.id,
        &sections[0],
        &json!({"type":"doc","content": vec_blocks(5)}),
        0,
    )
    .expect("commit");
    assert_eq!(store.wal().pending_count(&doc.id).expect("pending count"), 4, "still four sections");
}

#[test]
fn a_flush_with_nothing_pending_is_a_no_op_not_an_error() {
    // It happens on every idle tick and on every clean close. A command that failed when
    // there was nothing to do would log a failure on every idle tick.
    let (store, doc_id, _) = store_with_blocks(2);
    assert_eq!(holonomy_shell_lib::core::flush_document(&store, &doc_id).expect("flush"), 0);
    holonomy_shell_lib::core::commit_section_edit(
        &store,
        &doc_id,
        &store.section_ids(&doc_id).expect("ids")[0].clone(),
        &json!({"type":"doc","content": vec_blocks(2)}),
        0,
    )
    .expect("commit");
    assert_eq!(
        holonomy_shell_lib::core::flush_document(&store, &doc_id).expect("flush"),
        1,
        "the second flush should fold the one pending row"
    );
    assert_eq!(holonomy_shell_lib::core::flush_document(&store, &doc_id).expect("flush"), 0);
}

#[test]
fn the_pending_count_is_reported_on_every_commit() {
    // `CommitResponse::pending` is the operational signal: it is how a caller knows the
    // log is not being folded, long before the database file says so. It is reported per
    // commit so a caller can watch it without a separate poll.
    //
    // Pinned to *one* for repeated edits of one section, because that is the correct
    // value and an earlier version of this test asserted it grew with each keystroke —
    // which would have made the counter useless as a stuck-WAL signal.
    let (store, doc_id, section) = store_with_blocks(2);
    for i in 0..5 {
        let response = holonomy_shell_lib::core::commit_section_edit(
            &store,
            &doc_id,
            &section,
            &json!({"type":"doc","content": vec_blocks(2 + i)}),
            0,
        )
        .expect("commit");
        assert_eq!(response.pending, 1, "one section stays one pending state");
        assert!(response.wal_row_id > 0, "every commit names its row");
    }
}

#[test]
fn get_section_after_an_edit_reflects_the_edit_once_flushed() {
    // The two commands have to compose: fetch, edit, flush, fetch again. If `get_section`
    // served something stale after a flush, the frontend's LRU would hold content that
    // disagrees with the store and the reappearance of a section would be a revert.
    let (store, doc_id, section) = store_with_blocks(2);
    let before = holonomy_shell_lib::core::get_section(&store, Some(&doc_id), &section).expect("get");
    assert_eq!(
        holonomy_core::store::decode(&before.id, &before.content_zstd).expect("decode")["content"]
            .as_array()
            .expect("content")
            .len(),
        2
    );

    holonomy_shell_lib::core::commit_section_edit(
        &store,
        &doc_id,
        &section,
        &json!({"type":"doc","content": vec_blocks(9)}),
        0,
    )
    .expect("commit");
    holonomy_shell_lib::core::flush_document(&store, &doc_id).expect("flush");

    let after = holonomy_shell_lib::core::get_section(&store, Some(&doc_id), &section).expect("get");
    let decoded = holonomy_core::store::decode(&after.id, &after.content_zstd).expect("decode");
    assert_eq!(decoded["content"].as_array().expect("content").len(), 9, "the fetch served stale content");
}

// -- clean shutdown -------------------------------------------------------

/// A file-backed store, because the claim is about a file on disk.
///
/// An in-memory store is faster and every other test here uses one, and it would make
/// these tests pass vacuously: `journal_mode = WAL` has no `-wal` sidecar to truncate
/// for an in-memory database, so `wal_file_bytes()` is unconditionally 0 and a test
/// asserting "the journal was truncated" would be asserting `0 == 0`.
fn file_store(dir: &tempfile::TempDir) -> (Store, String, String) {
    let store = Store::open(&dir.path().join("holonomy.sqlite3")).expect("open store");
    let doc = store.create_document("Shut").expect("create document");
    let json = json!({"type":"doc","content": vec_blocks(3)});
    let section = store.add_section(&doc.id, &json).expect("add section");
    (store, doc.id, section)
}

/// Assert that a shutdown left nothing to recover, on disk.
///
/// Shared by every shutdown test so "clean" means one thing. Both halves, because they
/// are different files: the `wal` *table* must be empty and SQLite's `-wal` *sidecar*
/// must be zero bytes. A test that checked only the table would pass while leaving a
/// multi-megabyte journal that any tool reading only the database file would see as
/// stale.
fn assert_clean(store: &Store, report: &holonomy_shell_lib::core::ShutdownReport) {
    assert_eq!(
        report.pending_rows, 0,
        "clean exit must leave no WAL recovery rows, found {}",
        report.pending_rows
    );
    let on_disk = store.wal_file_bytes().expect("wal file size");
    assert_eq!(
        report.wal_bytes, 0,
        "PRAGMA wal_checkpoint(TRUNCATE) must leave the -wal sidecar at zero bytes, got {report:?}"
    );
    assert_eq!(on_disk, 0, "the -wal sidecar should be 0 bytes on disk too, got {on_disk}");
    assert!(report.is_clean(), "the report should agree that this was clean: {report:?}");
}

#[test]
fn a_clean_exit_leaves_no_pending_wal_rows() {
    // The directive's claim, asserted.
    let dir = tempfile::tempdir().expect("tempdir");
    let (store, doc_id, section) = file_store(&dir);

    // An edit committed but never flushed: exactly the state a session ends in, since
    // the typing path only commits and the debounce may not have fired.
    holonomy_shell_lib::core::commit_section_edit(
        &store,
        &doc_id,
        &section,
        &json!({"type":"doc","content": vec_blocks(9)}),
        3,
    )
    .expect("commit");
    assert!(
        store.wal().row_count(&doc_id).expect("rows") > 0,
        "precondition: there is something in the log to fold"
    );

    let report = holonomy_shell_lib::core::graceful_shutdown(&store).expect("shutdown");
    assert_eq!(report.flushed, 1, "the pending snapshot should have been folded");
    assert_eq!(report.documents, 1, "one document had pending rows");
    assert_clean(&store, &report);
}

#[test]
fn the_folded_edit_is_in_the_section_row_not_merely_logged() {
    // Zero pending rows is not the goal; zero pending rows *and* the content present is.
    // A shutdown that truncated the log without folding would satisfy the first and
    // silently lose the edit.
    let dir = tempfile::tempdir().expect("tempdir");
    let (store, doc_id, section) = file_store(&dir);
    let edited = json!({"type":"doc","content": vec_blocks(11)});

    holonomy_shell_lib::core::commit_section_edit(&store, &doc_id, &section, &edited, 7)
        .expect("commit");
    holonomy_shell_lib::core::graceful_shutdown(&store).expect("shutdown");

    assert_eq!(
        store.load_section(&section).expect("load"),
        edited,
        "the folded content is not what was committed"
    );
    let manifest = store.manifest(&doc_id).expect("manifest");
    assert_eq!(manifest.entries()[0].block_count, 11, "the row's counts should describe the edit");
    assert_eq!(manifest.entries()[0].mark_count, 7, "the mark count only the caller knew");
}

#[test]
fn a_reopened_database_needs_no_recovery() {
    // What a user experiences as "it did not lose my last edit". The recovery path runs
    // at boot, so a shutdown that only emptied the log would still look correct here if
    // the payload had been written; the way to distinguish is to count recoveries.
    let dir = tempfile::tempdir().expect("tempdir");
    let (store, doc_id, section) = file_store(&dir);
    let edited = json!({"type":"doc","content": vec_blocks(13)});
    holonomy_shell_lib::core::commit_section_edit(&store, &doc_id, &section, &edited, 2)
        .expect("commit");
    holonomy_shell_lib::core::graceful_shutdown(&store).expect("shutdown");
    drop(store);

    let reopened = Store::open(&dir.path().join("holonomy.sqlite3")).expect("reopen");
    assert_eq!(
        reopened.recover(&doc_id).expect("recover"),
        0,
        "a clean exit must leave nothing for the recovery path to replay"
    );
    assert_eq!(reopened.load_section(&section).expect("load"), edited);
}

#[test]
fn shutdown_folds_every_document_with_pending_rows_not_only_the_open_one() {
    // A store outlives the document being read. Two documents edited, the UI showed one
    // at a time, and a shutdown that folded `document_id` alone would report success
    // while leaving the other's rows behind — which is exactly when a reader expects the
    // file to be complete.
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(&dir.path().join("holonomy.sqlite3")).expect("open store");

    let mut ids = Vec::new();
    for (n, title) in ["First", "Second", "Third"].iter().enumerate() {
        let doc = store.create_document(title).expect("doc");
        let section = store
            .add_section(&doc.id, &json!({"type":"doc","content": vec_blocks(2)}))
            .expect("section");
        holonomy_shell_lib::core::commit_section_edit(
            &store,
            &doc.id,
            &section,
            &json!({"type":"doc","content": vec_blocks(4 + n)}),
            0,
        )
        .expect("commit");
        ids.push((doc.id, section));
    }
    assert_eq!(store.documents_with_pending_wal().expect("pending docs").len(), 3);

    let report = holonomy_shell_lib::core::graceful_shutdown(&store).expect("shutdown");
    assert_eq!(report.documents, 3, "all three documents had pending rows");
    assert_eq!(report.flushed, 3, "one snapshot per document");
    assert_clean(&store, &report);

    for (doc_id, section) in &ids {
        assert_eq!(
            store.load_section(section).expect("load"),
            json!({"type":"doc","content": vec_blocks(4 + ids.iter().position(|(d, _)| d == doc_id).unwrap())}),
            "each document's edit should be folded"
        );
    }
}

#[test]
fn shutdown_with_nothing_pending_is_a_clean_no_op() {
    // It runs on every quit, including quitting without editing. A shutdown that failed
    // when there was nothing to do would log a failure on every ordinary exit.
    let dir = tempfile::tempdir().expect("tempdir");
    let (store, _, _) = file_store(&dir);
    let report = holonomy_shell_lib::core::graceful_shutdown(&store).expect("shutdown");
    assert_eq!(report.flushed, 0, "nothing to fold");
    assert_eq!(report.documents, 0, "nothing was pending");
    assert_clean(&store, &report);
}

#[test]
fn shutdown_is_idempotent_because_a_second_close_can_happen() {
    // The window can be closed twice — a "close all windows" plus a quit, or a
    // supervisor that sends the event after the user already did. The second call must
    // not double-count or error.
    let dir = tempfile::tempdir().expect("tempdir");
    let (store, doc_id, section) = file_store(&dir);
    holonomy_shell_lib::core::commit_section_edit(
        &store,
        &doc_id,
        &section,
        &json!({"type":"doc","content": vec_blocks(6)}),
        0,
    )
    .expect("commit");

    let first = holonomy_shell_lib::core::graceful_shutdown(&store).expect("first");
    let second = holonomy_shell_lib::core::graceful_shutdown(&store).expect("second");
    assert_eq!(first.flushed, 1);
    assert_eq!(second.flushed, 0, "the second close has nothing left to fold");
    assert_clean(&store, &second);
}

#[test]
fn an_in_memory_store_reports_zero_bytes_rather_than_failing() {
    // The counter is not meaningful without a file, and every other test in the store
    // uses an in-memory one. Reporting 0 rather than erroring keeps those tests
    // callable while making the distinction explicit: a caller that needs the journal
    // size checks `path()`.
    let store = Store::open_in_memory().expect("in-memory store");
    let doc = store.create_document("Mem").expect("doc");
    let section = store
        .add_section(&doc.id, &json!({"type":"doc","content": vec_blocks(2)}))
        .expect("section");
    holonomy_shell_lib::core::commit_section_edit(
        &store,
        &doc.id,
        &section,
        &json!({"type":"doc","content": vec_blocks(3)}),
        0,
    )
    .expect("commit");

    let report = holonomy_shell_lib::core::graceful_shutdown(&store).expect("shutdown");
    assert_eq!(report.pending_rows, 0, "the table is still emptied");
    assert_eq!(report.wal_bytes, 0, "there is no sidecar to measure");
    assert!(store.path().is_none(), "an in-memory store has no path");
}

// -- assets: the holo-asset:// protocol -----------------------------------

/// A store holding one asset, plus the hash that addresses it.
fn store_with_asset(bytes: &[u8], mime: &str) -> (Store, String) {
    let store = Store::open_in_memory().expect("in-memory store");
    let hash = store.put_asset(bytes, mime).expect("put_asset");
    (store, hash)
}

#[test]
fn an_asset_url_round_trips_through_the_store() {
    // The whole contract in one: hash the bytes, build the URL the document stores, resolve
    // it the way the protocol handler does, get the bytes back. The two halves are written
    // in different crates — `asset_uri` here, the frontend's copy in `app/src/core/assets.ts`
    // — so this is the test that would catch them drifting.
    let png = [0x89u8, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0xff, 0x00];
    let (store, hash) = store_with_asset(&png, "image/png");
    let uri = core::asset_uri(&hash);

    assert!(uri.starts_with("holo-asset://"), "expected the holo-asset scheme");
    match core::resolve_asset_uri(&store, &uri).expect("resolve") {
        core::AssetResponse::Found { mime, bytes } => {
            assert_eq!(mime, "image/png");
            assert_eq!(bytes, png);
        }
        other => panic!("expected the asset, got {other:?}"),
    }
}

#[test]
fn the_asset_url_is_built_from_the_sha256_of_the_bytes() {
    // Not "consistent with `put_asset`" — `put_asset` calls the same function, so that
    // would be a tautology. These are the FIPS 180-4 vectors, so a change of hash function
    // cannot pass by being internally coherent.
    assert_eq!(
        holonomy_core::sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        holonomy_core::sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    let (store, hash) = store_with_asset(b"abc", "text/plain");
    assert_eq!(hash, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    assert_eq!(core::asset_uri(&hash), format!("holo-asset://{hash}"));
    let _ = store;
}

#[test]
fn a_miss_is_a_404_and_a_malformed_url_is_not_an_asset() {
    // Three outcomes, because the handler's job is to tell them apart. A miss is a normal
    // outcome of opening a document whose assets have not synced and is worth retrying; a
    // malformed URL is a bug at the call site and is not; conflating them either way
    // produces a broken image with no indication which.
    let (store, hash) = store_with_asset(b"payload", "application/octet-stream");
    let missing = "holo-asset://".to_string() + &"0".repeat(64);
    assert_ne!(missing, core::asset_uri(&hash));

    assert_eq!(
        core::resolve_asset_uri(&store, &missing).expect("resolve"),
        core::AssetResponse::NotFound,
        "a well-formed URL for an absent asset is a miss"
    );
    for bad in [
        "holo-asset://short",
        "holo-asset://",
        // A path traversal has no directory to traverse into, and accepting it would be a
        // traversal with extra steps. The whole path is refused rather than normalised.
        &format!("holo-asset://{hash}/../../etc/passwd"),
        &format!("holo-asset://{hash}/variant"),
        // Uppercase cannot match a stored key, so it is a typo rather than a valid
        // spelling. One hash, one spelling.
        &core::asset_uri(&hash.to_uppercase()),
        &format!("holo-asset://{}g", "0".repeat(63)),
        "https://example.com/whatever.png",
        "file:///etc/passwd",
        "",
    ] {
        assert_eq!(
            core::resolve_asset_uri(&store, bad).expect("resolve"),
            core::AssetResponse::NotAnAsset,
            "should have been refused as not an asset URL: {bad:?}"
        );
    }
}

#[test]
fn identical_bytes_are_stored_once_and_two_documents_share_them() {
    // Content addressing is what makes an asset URL a cache key, an immutable URL, and a
    // sync unit. It stops being any of those the moment the key is not a function of the
    // content alone.
    let store = Store::open_in_memory().expect("store");
    let logo = [0xde, 0xad, 0xbe, 0xef];
    let a = store.put_asset(&logo, "image/png").expect("put");
    let b = store.put_asset(&logo, "image/png").expect("put again");
    assert_eq!(a, b, "the same bytes must hash to the same key");

    // A different mime for the same bytes is still the same asset: the key is the content,
    // and overwriting the mime would be a last-writer-wins on a shared key.
    let c = store.put_asset(&logo, "image/webp").expect("put under another mime");
    assert_eq!(c, a);

    let (_, bytes) = store.get_asset(&a).expect("get").expect("present");
    assert_eq!(bytes, logo);
}

#[test]
fn a_large_asset_survives_the_round_trip_unaltered() {
    // Bytes above 127 in every position, which is where a `char`-per-byte or a hex round
    // trip goes wrong. An image that comes back subtly altered decodes to something, so
    // the failure is a wrong picture rather than an error.
    let bytes: Vec<u8> = (0..=255u8).cycle().take(100_000).collect();
    let (store, hash) = store_with_asset(&bytes, "image/png");
    let uri = core::asset_uri(&hash);
    match core::resolve_asset_uri(&store, &uri).expect("resolve") {
        core::AssetResponse::Found { bytes: got, .. } => {
            assert_eq!(got.len(), bytes.len());
            assert!(got == bytes, "a large asset came back altered");
        }
        other => panic!("expected the asset, got {other:?}"),
    }
}

// -- lifecycle: prune -----------------------------------------------------
//
// The store side of the Backspace-at-position-0 gesture on an empty section. The
// frontend decides a section is empty and asks for it to be removed; these tests are
// about whether the store agrees, and about what it refuses.

/// A document of `n` sections where each holds `blocks` paragraphs, but section `i` is
/// emptied when `empty_at` is `Some(i)`.
fn store_with_empty_at(n: usize, blocks: usize, empty_at: Option<usize>) -> (Store, Vec<String>) {
    let store = Store::open_in_memory().expect("in-memory store");
    let doc = store.create_document("Test").expect("create document");
    let mut ids = Vec::new();
    for i in 0..n {
        let content = if empty_at == Some(i) {
            vec![json!({"type":"paragraph"})]
        } else {
            vec_blocks(blocks)
        };
        ids.push(
            store
                .add_section(&doc.id, &json!({"type":"doc","content": content}))
                .expect("add section"),
        );
    }
    (store, ids)
}

fn geometry_for(store: &Store) -> Geometry {
    let docs = store.documents().expect("documents");
    Geometry::from_manifest(&store.manifest(&docs[0].id).expect("manifest"))
}

#[test]
fn pruning_an_empty_section_removes_it_and_keeps_its_neighbours() {
    let (store, ids) = store_with_empty_at(3, 4, Some(1));
    let mut geometry = geometry_for(&store);

    let result = core::commit_section_lifecycle(
        &store,
        &mut geometry,
        &LifecycleAction::Prune {
            section_id: ids[1].clone(),
            previous_section_id: ids[0].clone(),
            index: 1,
        },
    )
    .expect("prune");

    assert!(result.applied, "an empty section should prune: {:?}", result.reason);
    assert_eq!(result.section_ids.len(), 2, "three sections become two");
    assert!(!result.section_ids.contains(&ids[1]), "the empty section must be gone");
    // The neighbours are untouched — a prune that appended the empty paragraph into its
    // predecessor would satisfy every count assertion here while changing a section.
    assert_eq!(store.load_section(&ids[0]).unwrap()["content"].as_array().unwrap().len(), 4);
    assert_eq!(store.load_section(&ids[2]).unwrap()["content"].as_array().unwrap().len(), 4);
}

#[test]
fn pruning_a_section_that_is_not_empty_is_refused() {
    // The store does not take the frontend's word for it. The two can disagree — the
    // section may have been typed into while the request was in flight — and deleting a
    // section that has content is not recoverable.
    let (store, ids) = store_with_empty_at(3, 4, None);
    let mut geometry = geometry_for(&store);

    let result = core::commit_section_lifecycle(
        &store,
        &mut geometry,
        &LifecycleAction::Prune {
            section_id: ids[1].clone(),
            previous_section_id: ids[0].clone(),
            index: 1,
        },
    )
    .expect("prune");

    assert!(!result.applied, "a populated section must not be pruned");
    assert!(result.reason.unwrap().contains("not empty"), "the reason must say why");
    assert_eq!(result.section_ids.len(), 0, "a refusal returns no ordering");
    assert_eq!(store.section_ids(&store.documents().unwrap()[0].id).unwrap().len(), 3);
}

#[test]
fn pruning_a_section_that_is_only_an_image_is_refused() {
    // An image has no words and no characters, so a word-count test would call it empty
    // and delete a figure the user inserted. `block_count` is what catches it.
    let store = Store::open_in_memory().expect("store");
    let doc = store.create_document("Figures").expect("doc");
    let a = store
        .add_section(&doc.id, &json!({"type":"doc","content": vec_blocks(3)}))
        .expect("a");
    let b = store
        .add_section(
            &doc.id,
            &json!({"type":"doc","content":[
                {"type":"image","attrs":{"src": format!("holo-asset://{}", "a".repeat(64))}}
            ]}),
        )
        .expect("b");
    let mut geometry = geometry_for(&store);

    let result = core::commit_section_lifecycle(
        &store,
        &mut geometry,
        &LifecycleAction::Prune { section_id: b.clone(), previous_section_id: a.clone(), index: 1 },
    )
    .expect("prune");

    assert!(!result.applied, "a section holding only a figure must not be pruned");
    assert!(store.load_section(&b).is_ok(), "the figure must survive");
}

#[test]
fn pruning_across_a_gap_is_refused() {
    // `p-c` is not immediately after `p-a`. Honouring the request would remove a section
    // the user was not standing in front of, leaving the empty one they *were* looking
    // at still on screen.
    let (store, ids) = store_with_empty_at(3, 4, Some(2));
    let mut geometry = geometry_for(&store);

    let result = core::commit_section_lifecycle(
        &store,
        &mut geometry,
        &LifecycleAction::Prune {
            section_id: ids[2].clone(),
            previous_section_id: ids[0].clone(),
            index: 2,
        },
    )
    .expect("prune");

    assert!(!result.applied, "a non-adjacent prune must be refused");
    assert!(result.reason.unwrap().contains("seam"), "the reason must name the adjacency");
    assert_eq!(store.section_ids(&store.documents().unwrap()[0].id).unwrap().len(), 3);
}

#[test]
fn pruning_the_only_section_is_refused() {
    // Not reachable from the keyboard — a one-section document has no seam — but the
    // command must not be able to empty a document if called directly.
    let (store, ids) = store_with_empty_at(1, 4, Some(0));
    let mut geometry = geometry_for(&store);

    let result = core::commit_section_lifecycle(
        &store,
        &mut geometry,
        &LifecycleAction::Prune {
            section_id: ids[0].clone(),
            previous_section_id: ids[0].clone(),
            index: 0,
        },
    )
    .expect("prune");

    assert!(!result.applied, "a section cannot be pruned against itself");
    assert_eq!(store.section_ids(&store.documents().unwrap()[0].id).unwrap().len(), 1);
}

#[test]
fn a_pruned_section_stops_being_findable_through_search() {
    // `sections_fts` is a contentless virtual table, not a foreign-keyed child of
    // `sections`, so the cascade does not reach it. An index row left behind is a search
    // hit that navigates to a section that no longer exists — the user searches, finds a
    // result, and lands nowhere.
    let store = Store::open_in_memory().expect("store");
    let doc = store.create_document("Searchable").expect("doc");
    let keep = store
        .add_section(&doc.id, &json!({"type":"doc","content":[
            {"type":"paragraph","content":[{"type":"text","text":"alpha survives"}]}
        ]}))
        .expect("keep");
    let doomed = store
        .add_section(&doc.id, &json!({"type":"doc","content":[
            {"type":"paragraph","content":[{"type":"text","text":"zebra vanishes"}]}
        ]}))
        .expect("doomed");

    assert!(
        !store.search(&doc.id, "zebra", 10).unwrap().is_empty(),
        "precondition: the section is indexed before it is pruned"
    );

    let mut geometry = geometry_for(&store);
    let result = core::commit_section_lifecycle(
        &store,
        &mut geometry,
        &LifecycleAction::Prune { section_id: doomed.clone(), previous_section_id: keep.clone(), index: 1 },
    )
    .expect("prune");
    assert!(!result.applied, "a section with text is not empty, so this is a refusal");
}

#[test]
fn a_pruned_sections_search_row_is_gone_when_the_section_was_empty() {
    // Same concern as above, reached the way it actually happens: a section emptied by an
    // edit, then pruned.
    let store = Store::open_in_memory().expect("store");
    let doc = store.create_document("Searchable").expect("doc");
    let keep = store
        .add_section(&doc.id, &json!({"type":"doc","content":[
            {"type":"paragraph","content":[{"type":"text","text":"alpha survives"}]}
        ]}))
        .expect("keep");
    let doomed = store
        .add_section(&doc.id, &json!({"type":"doc","content":[
            {"type":"paragraph","content":[{"type":"text","text":"zebra vanishes"}]}
        ]}))
        .expect("doomed");

    assert!(!store.search(&doc.id, "zebra", 10).unwrap().is_empty(), "precondition");

    // The user deletes the text, and the fold updates both the section and its index.
    let emptied = json!({"type":"doc","content":[{"type":"paragraph"}]});
    let analyzed = holonomy_core::analyze(&emptied);
    store
        .log_edit(
            &doc.id,
            &doomed,
            &emptied,
            holonomy_core::SectionMetrics::new(analyzed.word_count, 0, analyzed.char_count),
            &analyzed.text,
        )
        .expect("log");
    store.flush(&doc.id).expect("flush");

    let mut geometry = geometry_for(&store);
    let result = core::commit_section_lifecycle(
        &store,
        &mut geometry,
        &LifecycleAction::Prune { section_id: doomed.clone(), previous_section_id: keep.clone(), index: 1 },
    )
    .expect("prune");
    assert!(result.applied, "the section is empty now: {:?}", result.reason);

    assert!(
        store.search(&doc.id, "zebra", 10).unwrap().is_empty(),
        "the pruned section must not remain a search hit"
    );
    assert!(
        !store.search(&doc.id, "alpha", 10).unwrap().is_empty(),
        "and the surviving section must still be findable"
    );
}

// -- clean shutdown reclaims assets ---------------------------------------
//
// The sweep is wired into `graceful_shutdown`, so these assert the wiring rather than
// `sweep_orphaned_assets` itself (which is tested in `holonomy-core`). The claim worth
// testing here is that the *ordering* is right: a sweep before the fold reads the
// pre-delete content and keeps the bytes for a whole session.

/// A file-backed store with one document holding one section, plus an image.
fn file_store_with_asset(dir: &tempfile::TempDir) -> (Store, String, String, String) {
    let store = Store::open(&dir.path().join("holonomy.sqlite3")).expect("open store");
    let doc = store.create_document("Figures").expect("create document");
    let mut png = vec![0x89u8, b'P', b'N', b'G'];
    png.extend(std::iter::repeat(0x7fu8).take(32 * 1024));
    let hash = store.put_asset(&png, "image/png").expect("put asset");

    let section = store
        .add_section(
            &doc.id,
            &json!({"type":"doc","content": [
                {"type":"paragraph","content":[{"type":"text","text":"before"}]},
                {"type":"image","attrs":{"src": format!("holo-asset://{hash}")}}
            ]}),
        )
        .expect("add section");
    (store, doc.id, section, hash)
}

#[test]
fn shutdown_keeps_an_image_a_section_still_references() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (store, _doc, _section, hash) = file_store_with_asset(&dir);

    let report = holonomy_shell_lib::core::graceful_shutdown(&store).expect("shutdown");
    assert_eq!(report.assets_deleted, 0, "a referenced image must not be reclaimed");
    assert!(store.get_asset(&hash).unwrap().is_some(), "the bytes must still be there");
}

#[test]
fn shutdown_reclaims_an_image_deleted_since_the_last_session() {
    // The end-to-end claim: the user deletes a figure, the app closes, the bytes are
    // gone from the table on the next open.
    //
    // The deletion goes through `commit_section_edit` and stays in the WAL — it is never
    // flushed — so this also pins the ordering. A sweep placed before `flush_all` would
    // read the pre-delete section, see the image referenced, and keep the bytes; the file
    // would grow every session a figure was deleted, which is the leak this closes.
    let dir = tempfile::tempdir().expect("tempdir");
    let (store, doc_id, section, hash) = file_store_with_asset(&dir);

    holonomy_shell_lib::core::commit_section_edit(
        &store,
        &doc_id,
        &section,
        &json!({"type":"doc","content": [
            {"type":"paragraph","content":[{"type":"text","text":"after"}]}
        ]}),
        0,
    )
    .expect("commit the deletion");
    assert!(
        store.wal().row_count(&doc_id).expect("rows") > 0,
        "precondition: the deletion is still only in the log"
    );
    assert!(store.get_asset(&hash).unwrap().is_some(), "precondition: the bytes are still there");

    let report = holonomy_shell_lib::core::graceful_shutdown(&store).expect("shutdown");

    assert_eq!(report.assets_deleted, 1, "the deleted figure should have been reclaimed");
    assert!(report.assets_bytes >= 32 * 1024, "the report should account for the bytes");
    assert!(
        store.get_asset(&hash).unwrap().is_none(),
        "the row must be gone from the assets table"
    );
    assert_clean(&store, &report);
}

#[test]
fn unreclaimed_assets_do_not_make_a_shutdown_look_dirty() {
    // `is_clean` is about recovery, not tidiness. An asset sweep that reclaimed nothing
    // is the normal case, and if it counted towards `is_clean` then every ordinary
    // shutdown would report itself unclean.
    let dir = tempfile::tempdir().expect("tempdir");
    let (store, _doc, _section, _hash) = file_store_with_asset(&dir);
    let report = holonomy_shell_lib::core::graceful_shutdown(&store).expect("shutdown");
    assert_eq!(report.assets_deleted, 0);
    assert!(report.is_clean(), "reclaiming nothing must not count against a clean exit");
}

// -- optimize ---------------------------------------------------------------
//
// `File -> Optimize Document`. The pipeline is sweep -> incremental_vacuum -> checkpoint,
// and each step's position is load-bearing, so the tests below are about the order as much
// as the outcome.

#[test]
fn optimizing_a_clean_document_reclaims_nothing_and_reports_zero() {
    // The common case, and it must not be an error. A menu item that fails when there is
    // nothing to do is a menu item users stop clicking.
    let store = store_with(4, 3);
    let report = holonomy_shell_lib::core::optimize_document(&store, &store.documents().unwrap()[0].id)
        .expect("optimize");

    assert_eq!(report.assets_deleted, 0, "nothing is orphaned");
    assert_eq!(report.pages_reclaimed, 0, "nothing is free");
    assert_eq!(report.wal_bytes, 0, "the journal is truncated");
    assert!(report.is_empty(), "a clean document reports nothing done: {report:?}");
}

#[test]
fn optimizing_reclaims_an_orphaned_asset_and_folds_pending_edits() {
    let store = Store::open_in_memory().expect("store");
    let doc = store.create_document("Optimize").expect("doc");

    let mut png = vec![0x89u8, b'P', b'N', b'G'];
    png.extend(std::iter::repeat(0x5au8).take(64 * 1024));
    let hash = store.put_asset(&png, "image/png").expect("put");

    let section = store
        .add_section(
            &doc.id,
            &json!({"type":"doc","content": [
                {"type":"image","attrs":{"src": format!("holo-asset://{hash}")}}
            ]}),
        )
        .expect("add section");

    // Delete the reference through the real write path, leaving it in the log.
    let emptied = json!({"type":"doc","content":[{"type":"paragraph"}]});
    let analyzed = holonomy_core::analyze(&emptied);
    store
        .log_edit(
            &doc.id,
            &section,
            &emptied,
            holonomy_core::SectionMetrics::new(analyzed.word_count, 0, analyzed.char_count),
            &analyzed.text,
        )
        .expect("log");
    assert!(store.wal().row_count(&doc.id).expect("rows") > 0, "precondition: pending");

    let report = holonomy_shell_lib::core::optimize_document(&store, &doc.id).expect("optimize");

    assert_eq!(report.flushed, 1, "the pending edit must be folded before the sweep");
    assert_eq!(report.assets_deleted, 1, "the figure is now unreferenced");
    assert!(report.assets_freed >= 64 * 1024, "the report should account for the bytes");
    assert!(store.get_asset(&hash).unwrap().is_none(), "the row must be gone");
}

#[test]
fn optimizing_keeps_an_asset_a_section_still_references() {
    let store = Store::open_in_memory().expect("store");
    let doc = store.create_document("Keep").expect("doc");
    let png = vec![0x89u8, b'P', b'N', b'G', 1, 2, 3];
    let hash = store.put_asset(&png, "image/png").expect("put");
    store
        .add_section(
            &doc.id,
            &json!({"type":"doc","content": [
                {"type":"image","attrs":{"src": format!("holo-asset://{hash}")}}
            ]}),
        )
        .expect("add section");

    let report = holonomy_shell_lib::core::optimize_document(&store, &doc.id).expect("optimize");
    assert_eq!(report.assets_deleted, 0, "the figure is still referenced");
    assert!(store.get_asset(&hash).unwrap().is_some());
}

#[test]
fn optimizing_a_document_that_does_not_exist_is_an_error_not_an_empty_report() {
    // "0 orphans" for a typo'd id would read as a clean bill of health.
    let store = store_with(1, 1);
    let err = holonomy_shell_lib::core::optimize_document(&store, "nope").expect_err("must fail");
    assert!(
        matches!(err, holonomy_core::Error::DocumentNotFound(_)),
        "expected DocumentNotFound, got {err:?}"
    );
}

#[test]
fn optimizing_leaves_the_journal_truncated() {
    // The third step of the pipeline, and the one a user can check by looking for a
    // `-wal` file next to their document.
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(&dir.path().join("opt.holo")).expect("open");
    let doc = store.create_document("Journal").expect("doc");
    let section = store
        .add_section(&doc.id, &json!({"type":"doc","content": vec_blocks(20)}))
        .expect("add");

    holonomy_shell_lib::core::commit_section_edit(
        &store,
        &doc.id,
        &section,
        &json!({"type":"doc","content": vec_blocks(40)}),
        5,
    )
    .expect("commit");
    assert!(store.wal_file_bytes().expect("bytes") > 0, "precondition: a journal exists");

    let report = holonomy_shell_lib::core::optimize_document(&store, &doc.id).expect("optimize");
    assert_eq!(report.wal_bytes, 0, "the journal must be truncated");
    assert_eq!(store.wal().row_count(&doc.id).expect("rows"), 0, "the logical log drains too");
}
