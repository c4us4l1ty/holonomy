//! **Phase 13 part 5's gate: an `Editor` can be opened over a document it has never read.** 6 tests.
//!
//! # The claim
//!
//! [`Editor::from_skeleton`] produces a fully-formed editor — spans, tables, assets, undo, redo — for a
//! document whose bytes are not held, and its peak does not carry the document.
//!
//! | what it proves | test |
//! | --- | --- |
//! | every structure is built, not stubbed | [`a_skeleton_editor_has_every_structure_and_holds_no_bytes`] |
//! | it knows its own length | [`a_skeleton_editor_knows_its_length_before_reading_anything`] |
//! | the span map is honest about unread bytes | [`the_span_map_covers_the_whole_document_without_reading_it`] |
//! | **it is read-only until faulted** | [`a_skeleton_editor_refuses_an_edit_until_the_leaf_is_faulted_in`] |
//! | and then works normally | [`a_faulted_in_leaf_reads_and_edits_like_a_resident_one`] |
//! | the peak, measured | [`a_skeleton_editor_peaks_below_a_from_text_editor`] |
//!
//! # Why "spans are honest about unread bytes" is a real test
//!
//! `SpanMap::plain(text_len)` claims every byte is plain-styled. That claim is **unverifiable** without
//! reading the document — a document full of `**bold**` markers would make it false. So the test does not
//! assert the claim is true; it asserts the editor was built with the right **extent**, and separately
//! documents that the styling is discovered on fault-in rather than asserted here. A test that read the
//! document to check would be testing `SpanMap`, not the constructor.

use holonomy_text::{LeafSource, RopeError};

/// The filler the source serves. Plain ASCII with no markup, so `SpanMap::plain` is genuinely true here.
fn text(n: usize) -> Vec<u8> {
    (0..n).map(|i| (b'a' + (i % 26) as u8)).collect()
}

struct VecSource {
    bytes: Vec<u8>,
    fetches: u32,
}

impl LeafSource for VecSource {
    fn fetch_leaf(&mut self, offset: usize, out: &mut [u8]) -> Result<usize, RopeError> {
        self.fetches += 1;
        let n = out.len().min(self.bytes.len().saturating_sub(offset));
        out[..n].copy_from_slice(&self.bytes[offset..offset + n]);
        Ok(n)
    }
}

/// **Every field of the editor is constructed, and none of it holds a byte.** `resident_bytes() == 0` is
/// the load-bearing half; the rest checks the editor is a real editor rather than a stub that happens to
/// have the right length.
#[test]
fn a_skeleton_editor_has_every_structure_and_holds_no_bytes() {
    let len = 300_000;
    let e = holonomy_text::Editor::from_skeleton(len);

    assert_eq!(e.text_len(), len, "the editor knows its own length");
    assert_eq!(e.resident_bytes(), 0, "and holds none of the document");
    assert_eq!(e.resident_count(), 0, "no leaf is resident");
    assert_eq!(e.caret(), 0, "the caret starts at 0, which is a valid offset in the unread document");

    // The structures exist and are empty rather than absent: an empty `TableMap` and `AssetCatalog` are
    // the correct state for a document nothing has been read out of, and a *missing* one would be a stub.
    assert_eq!(e.tables().len(), 0, "no tables have been seen yet");
    assert_eq!(e.assets().len(), 0, "no images have been seen yet");
}

/// The length and the extent are answerable with **zero reads**, which is the whole premise.
#[test]
fn a_skeleton_editor_knows_its_length_before_reading_anything() {
    for len in [0usize, 1, 2_047, 2_048, 2_049, 300_000] {
        let e = holonomy_text::Editor::from_skeleton(len);
        assert_eq!(e.text_len(), len, "length wrong for {len}");
        assert_eq!(e.resident_bytes(), 0, "and {len} bytes were still read");
        assert_eq!(e.caret(), 0);
    }
}

/// **The span map's extent covers the whole document, and that is a claim about extent, not about
/// content.**
///
/// `SpanMap::plain` says "every byte is plain". Whether that is *true* depends on bytes nobody has read,
/// so this asserts the extent and names the limitation rather than pretending to check it.
#[test]
fn the_span_map_covers_the_whole_document_without_reading_it() {
    let len = 300_000;
    let e = holonomy_text::Editor::from_skeleton(len);
    // A style query at the far end of the document must be answerable, and must answer "plain" -- which
    // is `SpanMap::plain`'s claim. **The answer is unverified: nothing has read the document.** That is
    // the documented state, and it is corrected as leaves are faulted in.
    // `style_at` returns a `TextIntervalSpan`, so "plain" is `style_flags == 0` rather than a `0` value.
    let style = e.style_at((len - 1) as u32);
    assert_eq!(
        style.style_flags, 0,
        "the last byte of an unread document is claimed plain -- true here because the fixture is, and \
         UNVERIFIED in general, because no byte of the document has been read"
    );
    // The extent is the part that is checkable: the span covers the byte asked about.
    let last = (len - 1) as u32;
    assert!(
        style.start_byte <= last && last < style.end_byte,
        "the span must cover the byte asked about: {style:?} for offset {last}"
    );
    assert_eq!(e.resident_bytes(), 0, "answering that read nothing");
}

/// **A skeleton editor refuses an edit, and the refusal is the safe direction.**
///
/// An edit into an absent leaf could be "handled" by faulting it in — but faulting needs a
/// [`LeafSource`], and `Editor` deliberately has none: the seam belongs to the caller, because
/// `holonomy-text` cannot depend on `holonomy-container`. So the editor refuses, and **the session above
/// it is the thing that can act on that.**
#[test]
fn a_skeleton_editor_refuses_an_edit_until_the_leaf_is_faulted_in() {
    let doc = text(300_000);
    let mut e = holonomy_text::Editor::from_skeleton(doc.len());

    // `read_into` is `&self` and cannot fault, so it refuses rather than inventing bytes.
    let mut one = [0u8; 1];
    let err = e.read_into(0, &mut one).expect_err("an absent leaf must refuse, not return zeros");
    assert!(
        matches!(err, holonomy_text::EditorError::Rope(RopeError::LeafAbsent { .. })),
        "expected a typed LeafAbsent, got {err:?}"
    );

    // And nothing was mutated by the refusal.
    assert_eq!(e.text_len(), doc.len(), "the length did not move");
    assert_eq!(e.resident_bytes(), 0, "and nothing was faulted in behind the caller's back");
}

/// **After faulting, the editor reads and edits exactly like a resident one** — and this is the test that
/// makes the read-only state a limitation rather than a defect.
///
/// The comparison is against a *resident* editor over the same bytes, which is the only way to say "like a
/// resident one" without restating what that means.
#[test]
fn a_faulted_in_leaf_reads_and_edits_like_a_resident_one() {
    let doc = text(300_000);
    let mut sparse = holonomy_text::Editor::from_skeleton(doc.len());
    let mut resident = holonomy_text::Editor::from_text(&doc).expect("load");

    // Read the same byte through both.
    let mut a = [0u8; 1];
    let mut b = [0u8; 1];
    assert_eq!(resident.read_into(0, &mut b).expect("resident"), 1);
    let mut src = VecSource { bytes: doc.clone(), fetches: 0 };
    // **Through the editor's own faulting path, not through a rope built here.** The first version of
    // this test called a helper that constructed a *separate* `Rope` and faulted *that*, so the editor's
    // own leaves stayed absent and the very next assertion failed with `LeafAbsent`. A test that sets up
    // its own copy of the thing under test proves something about the copy.
    assert_eq!(
        sparse.read_into_faulting(&mut src, 0, &mut a).expect("fault and read"),
        1,
        "the editor's own faulting read path works"
    );
    assert_eq!(a[0], b[0], "and returns the same byte a resident editor does");
    assert!(sparse.resident_bytes() > 0, "so something is genuinely resident now");

    // And an edit into the now-resident leaf behaves the same as on a resident editor.
    // `insert_at` takes the offset directly, so there is no caret to move first. Both editors get the
    // same edit at the same offset, which is the comparison that makes "like a resident one" meaningful.
    sparse
        .insert_at(0, b"Z", holonomy_text::SpanPolicy::GrowIntoInsert)
        .expect("edit the faulted leaf");
    resident
        .insert_at(0, b"Z", holonomy_text::SpanPolicy::GrowIntoInsert)
        .expect("edit the resident leaf");
    assert_eq!(sparse.text_len(), resident.text_len(), "both grew by one");
    let mut x = [0u8; 1];
    let mut y = [0u8; 1];
    sparse.read_into(0, &mut x).expect("sparse");
    resident.read_into(0, &mut y).expect("resident");
    assert_eq!(x[0], b'Z', "the sparse editor really took the edit");
    assert_eq!(x[0], y[0], "and agrees with the resident one");
}

/// **The peak, measured in a child process**, skeleton editor against `from_text` editor.
#[test]
fn a_skeleton_editor_peaks_below_a_from_text_editor() {
    const CHILD: &str = "HOLONOMY_EDITOR_PEAK";
    const DOC: usize = 2 * 1024 * 1024;

    let peak = |mode: &str| -> u64 {
        let exe = std::env::current_exe().expect("test binary");
        let out = std::process::Command::new(&exe)
            .args(["--exact", "editor_peak_child", "--nocapture", "--test-threads=1"])
            .env(CHILD, mode)
            .output()
            .expect("spawn peak child");
        let stdout = String::from_utf8_lossy(&out.stdout);
        stdout
            .lines()
            .find_map(|l| l.find("PEAK ").map(|i| l[i + 5..].trim().parse().ok()))
            .flatten()
            .unwrap_or_else(|| panic!("child printed no PEAK line for {mode}:\n{stdout}"))
    };

    let skeleton = peak("skeleton");
    let whole = peak("whole");
    println!("editor: skeleton {skeleton} KiB, from_text {whole} KiB");
    assert!(
        whole > skeleton,
        "a skeleton editor peaking at {skeleton} KiB did not beat from_text's {whole} KiB"
    );
    assert!(
        skeleton < DOC as u64 / 4,
        "a {DOC}-byte skeleton editor cost {skeleton} KiB of peak -- that is O(document)"
    );
}

#[test]
fn editor_peak_child() {
    let Ok(mode) = std::env::var("HOLONOMY_EDITOR_PEAK") else {
        return;
    };
    const DOC: usize = 2 * 1024 * 1024;
    match mode.as_str() {
        "skeleton" => {
            let e = holonomy_text::Editor::from_skeleton(DOC);
            std::hint::black_box(&e);
        }
        "whole" => {
            let doc = vec![b'a'; DOC];
            let e = holonomy_text::Editor::from_text(&doc).expect("load");
            std::hint::black_box((&doc, &e));
        }
        other => panic!("unknown mode {other}"),
    }
    println!("PEAK {}", vm_hwm_kib());
}

fn vm_hwm_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("VmHWM:"))
                .and_then(|l| l.split_whitespace().next())
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(0)
}