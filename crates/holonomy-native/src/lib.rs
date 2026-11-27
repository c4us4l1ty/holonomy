//! Native frontend session boundary: existing encrypted storage and editing semantics, with
//! document glyphs exported as vector geometry rather than framework-owned plaintext strings.

use holonomy_seal::{CanarySet, ReaperGuard, SecretArenas, TripReason, Tripwire};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use holonomy_eye::{raster::FontSet, Fixed, Point, Shaper};
use holonomy_gui::store::{self, Document, DocumentSink, DocumentSource};
use holonomy_shell::{Editor, Intent};
use holonomy_text::{ResidentSet, Typing};
use zeroize::Zeroize;

/// Fallible session operation. Errors retain the storage layer's coarse open refusal.
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

type Core = Editor<DocumentSink, DocumentSource, FontSet>;

/// A glyph's closed contours, already positioned in logical viewport coordinates.
///
/// There are no document strings in this representation. It is not a claim that GPU-visible
/// geometry cannot reveal text; scene and GPU residency still need lifecycle qualification.
pub struct VectorGlyph {
    /// Closed contours with downward-positive screen coordinates.
    pub contours: Vec<Vec<(f32, f32)>>,
}

impl Drop for VectorGlyph {
    fn drop(&mut self) {
        for contour in &mut self.contours {
            contour.zeroize();
        }
    }
}

/// A logical rectangle for selection, decoration or caret painting.
#[derive(Clone, Copy, Debug)]
pub struct BoxRect {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width.
    pub width: f32,
    /// Height.
    pub height: f32,
}

/// A bounded frame without plaintext copies in GPUI text elements or layout caches.
#[derive(Default)]
pub struct VectorFrame {
    /// Positioned glyph outlines.
    pub glyphs: Vec<VectorGlyph>,
    /// Selected runs from the same layout used for hit testing.
    pub selection: Vec<BoxRect>,
    /// Underlines and strikethroughs.
    pub decorations: Vec<BoxRect>,
    /// Visible caret, if any.
    pub caret: Option<BoxRect>,
}

/// One open encrypted document. A failed save never replaces or closes this object.
pub struct Session {
    path: PathBuf,
    document: Document,
    editor: Core,
    fonts: FontSet,
    width: Fixed,
    height: Fixed,
    history: Option<Core>,
    emergency: Emergency,
}

// No editor/key pointers are registered while the UI may mutate or reallocate them. A cooperative
// reaper latches the trip but waits for the UI thread to register quiescent allocations and join.
struct Emergency {
    reaper: Option<ReaperGuard>,
    wipe_verified: bool,
    _canaries: Arc<CanarySet>,
    arenas: Arc<SecretArenas>,
    tripwire: Arc<Tripwire>,
}

impl Emergency {
    fn new(timeout: Option<Duration>) -> Self {
        let arenas = SecretArenas::new();
        let mut canaries = CanarySet::new(8, 256);
        canaries.arm_all(&arenas);
        let canaries = Arc::new(canaries);
        let tripwire = Tripwire::new(timeout);
        tripwire.beat();
        let reaper = ReaperGuard::spawn_cooperative(
            Arc::clone(&arenas),
            Arc::clone(&canaries),
            Arc::clone(&tripwire),
            Duration::from_millis(50),
        );
        Self {
            reaper: Some(reaper),
            wipe_verified: false,
            _canaries: canaries,
            arenas,
            tripwire,
        }
    }
}

fn core(
    document: &Document,
    spine: holonomy_text::Spine,
    fonts: FontSet,
    width: Fixed,
    height: Fixed,
) -> Result<Core> {
    let residency = ResidentSet::new(document.source(), store::BUDGET, store::SLACK)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok(Editor::new(
        Typing::resume(document.sink(), spine, store::CAPACITY),
        residency,
        fonts,
        width,
        height,
    ))
}

impl Session {
    /// Opens an existing document; never falls back to creating one after an open failure.
    pub fn open(path: &Path, passphrase: &[u8]) -> Result<Self> {
        require_durability()?;
        Self::from_document(path, Document::open(path, passphrase)?)
    }

    /// Creates a new document, refusing an existing destination.
    pub fn create(path: &Path, passphrase: &[u8]) -> Result<Self> {
        require_durability()?;
        Self::from_document(path, Document::create(path, passphrase)?)
    }

    /// Adapts an authenticated document without reading its complete plaintext.
    pub fn from_document(path: &Path, document: Document) -> Result<Self> {
        let fonts = FontSet::embedded(17)?;
        let width = Fixed::units(720);
        let height = Fixed::units(520);
        let editor = core(&document, document.spine()?, fonts.clone(), width, height)?;
        Ok(Self {
            path: path.to_path_buf(),
            document,
            editor,
            fonts,
            width,
            height,
            history: None,
            emergency: Emergency::new(None),
        })
    }

    /// Metadata path only; no document body is read.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether a historical, read-only view is active.
    pub fn in_history(&self) -> bool {
        self.history.is_some()
    }

    /// Whether there is data not yet committed to a witness.
    pub fn dirty(&self) -> bool {
        !self.editor.typing().pending().is_empty()
            || self.editor.typing().has_uncommitted_structure()
    }

    /// Applies the same intent as the existing frontend. History views cannot mutate storage.
    pub fn apply(&mut self, intent: Intent) -> Result<()> {
        self.activity()?;
        if matches!(intent, Intent::SetView(view) if view != holonomy_eye::View::IDENTITY) {
            return Err("Native view transforms are not qualified; device scaling is handled by GPUI.".into());
        }
        if let Some(history) = self.history.as_mut() {
            if !matches!(
                intent,
                Intent::Click(_)
                    | Intent::DragTo(_)
                    | Intent::ScrollBy(_)
                    | Intent::ScrollTo(_)
                    | Intent::Move { .. }
                    | Intent::SelectAll
                    | Intent::RevealCaret
            ) {
                return Err("History is read-only; return to the current document to edit.".into());
            }
            history.apply(intent)?;
        } else {
            self.editor.apply(intent)?;
        }
        Ok(())
    }

    /// Commits through the existing authenticated manifest/witness path.
    pub fn save(&mut self) -> Result<()> {
        self.activity()?;
        self.editor.flush()?;
        Ok(())
    }

    /// Flushes before leaving, so a refusal keeps the session available for recovery.
    pub fn prepare_close(&mut self) -> Result<()> {
        self.save()
    }

    /// Resizes both views using logical coordinates, independently of the monitor scale.
    pub fn resize(&mut self, width: f32, height: f32) {
        if self.tripped() {
            return;
        }
        self.width = fixed(width.max(40.));
        self.height = fixed(height.max(40.));
        self.editor.resize(self.width, self.height);
        if let Some(history) = &mut self.history {
            history.resize(self.width, self.height);
        }
    }

    /// Finds and selects one streaming search result without collecting every match.
    pub fn find(&mut self, needle: &str, index: u64) -> Result<u64> {
        self.activity()?;
        let editor = self.history.as_mut().unwrap_or(&mut self.editor);
        let summary = editor.find_nth(needle, index)?;
        if let Some(hit) = summary.selected {
            editor.select_range(hit.start, hit.end)?;
            editor.apply(Intent::RevealCaret)?;
        }
        Ok(summary.count)
    }

    /// Returns checkpoint sequence numbers; listing history does not decrypt the document body.
    pub fn witnesses(&self) -> Vec<u64> {
        self.document
            .witness_history()
            .iter()
            .map(|w| w.sequence)
            .collect()
    }

    /// Shows a committed checkpoint without replacing the live editor or its undo history.
    pub fn show_witness(&mut self, sequence: u64) -> Result<()> {
        self.save()?;
        let next = core(
            &self.document,
            self.document.spine_at_witness(sequence)?,
            self.fonts.clone(),
            self.width,
            self.height,
        )?;
        if let Some(mut old) = self.history.replace(next) {
            old.wipe_all_in_place();
        }
        Ok(())
    }

    /// Discards only the read-only history viewport.
    pub fn show_current(&mut self) {
        if let Some(mut history) = self.history.take() {
            history.wipe_all_in_place();
        }
    }

    /// Adds a bookmark only after a successful save.
    pub fn bookmark(&mut self, name: &str) -> Result<()> {
        self.save()?;
        let sequence = self
            .witnesses()
            .last()
            .copied()
            .ok_or("No committed witness to bookmark")?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs()
            .min(i64::MAX as u64) as i64;
        self.document.add_bookmark(sequence, now, name)?;
        Ok(())
    }

    /// Authenticates the stored world-line without changing the current editor.
    pub fn verify(&self) -> Result<()> {
        self.check_live()?;
        self.document.verify()?;
        Ok(())
    }

    /// Volatile-wipes resident editor buffers and unlocked storage keys. The caller must also
    /// discard the GPU scene and its own input buffers; this does not certify GPU memory erasure.
    pub fn wipe(&mut self) -> bool {
        let Some(reaper) = self.emergency.reaper.take() else {
            return self.emergency.wipe_verified;
        };
        self.emergency.tripwire.fire(TripReason::Manual);
        let arenas = &self.emergency.arenas;
        // SAFETY: the UI holds exclusive access, no further document/editor work is performed
        // until wait_outcome joins the cooperative reaper. Each registered allocation stays live.
        unsafe {
            self.document.arm_reaper(arenas);
            self.editor
                .set_wipe_registry(Arc::new(holonomy_gui::SealWipeBridge(Arc::clone(arenas))));
            if let Some(history) = &mut self.history {
                history
                    .set_wipe_registry(Arc::new(holonomy_gui::SealWipeBridge(Arc::clone(arenas))));
            }
            reaper.acknowledge_quiescence();
        }
        let outcome = reaper.wait_outcome();
        self.editor.wipe_all_in_place();
        if let Some(history) = &mut self.history {
            history.wipe_all_in_place();
        }
        let verified = self.document.are_keys_zeroized()
            && outcome.is_some_and(|result| {
                result.canaries.ghost_canaries == 0
                    && result.wipe.bytes_wiped > 0
                    && result.wipe.arenas_wiped > 0
            });
        self.emergency.wipe_verified = verified;
        verified
    }

    /// Enables the existing cooperative dead-man watcher. Call before exposing the session.
    pub fn set_deadman(&mut self, timeout: Duration) -> Result<()> {
        self.check_live()?;
        if timeout.is_zero() {
            return Err("Dead-man timeout must be positive.".into());
        }
        self.emergency = Emergency::new(Some(timeout));
        Ok(())
    }

    /// Reports a permanently latched manual/dead-man trip without reading document plaintext.
    pub fn tripped(&self) -> bool {
        self.emergency.reaper.is_none() || self.emergency.tripwire.evaluate().is_some()
    }

    /// Records real user activity without reviving a deadline that has already expired.
    /// Paint, timer polling and other background work must never call this method.
    pub fn activity(&self) -> Result<()> {
        self.emergency.tripwire.beat();
        self.check_live()
    }

    fn check_live(&self) -> Result<()> {
        if self.tripped() {
            Err("Session emergency trip is latched; wiping before further access.".into())
        } else {
            Ok(())
        }
    }

    /// Computes toolbar formatting from the core selection, never from a second document model.
    pub fn toolbar(&self) -> holonomy_text::ToolbarState {
        self.editor.toolbar_state()
    }

    /// Copies the explicitly selected plaintext for an explicit clipboard operation only.
    /// The caller owns clipboard disclosure and cleanup policy.
    pub fn selected_text(&mut self) -> Result<zeroize::Zeroizing<String>> {
        self.activity()?;
        let editor = self.history.as_mut().unwrap_or(&mut self.editor);
        let (start, end) = editor.selection().normalised();
        if end.saturating_sub(start) > 1024 * 1024 {
            return Err("Selection exceeds the 1 MiB clipboard limit.".into());
        }
        Ok(zeroize::Zeroizing::new(
            editor.copy_selection(1024 * 1024)?.unwrap_or_default(),
        ))
    }

    /// Paints positioned outlines directly, preserving the core's shaping/hit-test geometry.
    pub fn frame(&mut self, device_scale: f32) -> Result<VectorFrame> {
        self.check_live()?;
        let editor = self.history.as_mut().unwrap_or(&mut self.editor);
        let frame = editor.frame()?;
        let picture = frame.picture();
        let mut output = VectorFrame::default();
        for span in &picture.selection {
            if let Some(line) = picture.lines.get(span.line) {
                output.selection.push(BoxRect {
                    x: span.x.to_f32(),
                    y: line.top.to_f32(),
                    width: (span.end - span.x).to_f32(),
                    height: line.height.to_f32(),
                });
            }
        }
        if let Some(caret) = picture.caret {
            output.caret = Some(BoxRect {
                x: caret.x.to_f32(),
                y: caret.top.to_f32(),
                width: 1.,
                height: caret.height.to_f32(),
            });
        }
        append_layout(
            &mut output,
            frame.layout(),
            &self.fonts,
            device_scale,
            self.height.to_f32(),
        )?;
        Ok(output)
    }

    /// Routes a logical viewport click or drag through the editor's single geometry.
    pub fn pointer(&mut self, x: f32, y: f32, extend: bool) -> Result<()> {
        let point = Point {
            x: fixed(x),
            y: fixed(y),
        };
        self.apply(if extend {
            Intent::DragTo(point)
        } else {
            Intent::Click(point)
        })
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(history) = &mut self.history {
            history.wipe_all_in_place();
        }
        self.editor.wipe_all_in_place();
        // Document's locked key owners zeroize on drop. No silent best-effort save in Drop:
        // callers must handle prepare_close failures before deciding to release this session.
    }
}

/// Converts UI coordinates at the one fixed-point boundary shared by paint and input.
pub fn fixed(value: f32) -> Fixed {
    if !value.is_finite() {
        return Fixed::ZERO;
    }
    Fixed::from_64ths(
        (f64::from(value) * 64.)
            .round()
            .clamp(i64::MIN as f64, i64::MAX as f64) as i64,
    )
}

/// Produces vector geometry for a bounded sensitive input field without using GPUI text caches.
pub fn vector_text(
    fonts: &FontSet,
    text: &str,
    width: f32,
    device_scale: f32,
) -> Result<VectorFrame> {
    let layout = holonomy_eye::layout_window(text, 0, fixed(width.max(40.)), fonts);
    let mut output = VectorFrame::default();
    append_layout(&mut output, &layout, fonts, device_scale, 80.)?;
    if let Some(caret) = layout.paint_point(text.len() as u64) {
        output.caret = Some(BoxRect {
            x: caret.x.to_f32(),
            y: caret.top.to_f32(),
            width: 1.,
            height: caret.height.to_f32(),
        });
    }
    Ok(output)
}

fn append_layout(
    output: &mut VectorFrame,
    layout: &holonomy_eye::Layout<'_>,
    fonts: &FontSet,
    device_scale: f32,
    visible_height: f32,
) -> Result<()> {
    for line in layout.lines() {
        if line.top().to_f32() >= visible_height || (line.top() + line.height()).to_f32() <= 0. {
            continue;
        }
        let clusters = line.clusters();
        let mut start = 0;
        while start < clusters.len() {
            let style = clusters[start].style;
            let mut end = start + 1;
            while end < clusters.len() && clusters[end].style == style {
                end += 1;
            }
            let run = &clusters[start..end];
            let rtl = run.len() >= 2 && run[0].byte > run[run.len() - 1].byte;
            let slices: Vec<&str> = if rtl {
                run.iter().rev().map(|c| c.text).collect()
            } else {
                run.iter().map(|c| c.text).collect()
            };
            let baseline = line.top().to_f32() + fonts.ascent(style).to_f32();
            let mut last_cluster = usize::MAX;
            let mut pen = 0.;
            for (variant, glyph) in fonts.shape_run(&slices, style) {
                if glyph.cluster >= run.len() {
                    continue;
                }
                if glyph.cluster != last_cluster {
                    last_cluster = glyph.cluster;
                    pen = 0.;
                }
                let span = glyph.cluster_span.max(1).min(run.len() - glyph.cluster);
                let visual = if rtl {
                    run.len() - (glyph.cluster + span)
                } else {
                    glyph.cluster
                };
                let x = run[visual].x.to_f32()
                    + pen
                    + fonts.scale_units(variant, glyph.x_offset, style).to_f32();
                let y = baseline - fonts.scale_units(variant, glyph.y_offset, style).to_f32();
                if let Some(mut outline) =
                    fonts.vector_glyph(variant, glyph.glyph_id, style, device_scale)?
                {
                    for contour in &mut outline.contours {
                        for point in contour {
                            point.0 += x;
                            point.1 = y - point.1;
                        }
                    }
                    output.glyphs.push(VectorGlyph {
                        contours: outline.contours,
                    });
                }
                pen += fonts.scale_units(variant, glyph.x_advance, style).to_f32();
            }
            let width = (run.last().unwrap().end() - run[0].x).to_f32();
            if style.is_underline() {
                output.decorations.push(BoxRect {
                    x: run[0].x.to_f32(),
                    y: baseline + 2.,
                    width,
                    height: 1.,
                });
            }
            if style.is_strikethrough() {
                output.decorations.push(BoxRect {
                    x: run[0].x.to_f32(),
                    y: baseline - fonts.ascent(style).to_f32() / 3.,
                    width,
                    height: 1.,
                });
            }
            start = end;
        }
    }
    Ok(())
}

/// Fixed-capacity, zeroizing UTF-8 input. Edits never reallocate a passphrase buffer and deleting
/// a character immediately clears its old bytes. No Debug/Display implementation exposes it.
pub struct SecretInput {
    storage: zeroize::Zeroizing<Vec<u8>>,
    len: usize,
}

impl Default for SecretInput {
    fn default() -> Self {
        Self {
            storage: zeroize::Zeroizing::new(vec![0; 4096]),
            len: 0,
        }
    }
}

impl SecretInput {
    /// Appends valid UTF-8 if it fits, leaving the old value unchanged on refusal.
    pub fn push_str(&mut self, text: &str) -> bool {
        let Some(end) = self
            .len
            .checked_add(text.len())
            .filter(|&end| end <= self.storage.len())
        else {
            return false;
        };
        self.storage[self.len..end].copy_from_slice(text.as_bytes());
        self.len = end;
        true
    }
    /// Replaces an exact UTF-16 range in place, refusing split surrogates or a capacity overflow.
    /// A refusal leaves the old input unchanged, including its marked composition.
    pub fn replace_utf16(&mut self, range: std::ops::Range<usize>, text: &str) -> bool {
        let Some(bytes) = utf16_range_bytes(self, range) else {
            return false;
        };
        let old_len = self.len;
        let remaining = old_len - (bytes.end - bytes.start);
        let Some(new_len) = remaining
            .checked_add(text.len())
            .filter(|&n| n <= self.storage.len())
        else {
            return false;
        };
        self.storage
            .copy_within(bytes.end..old_len, bytes.start + text.len());
        self.storage[bytes.start..bytes.start + text.len()].copy_from_slice(text.as_bytes());
        if new_len < old_len {
            self.storage[new_len..old_len].zeroize();
        }
        self.len = new_len;
        true
    }

    /// Removes the final grapheme cluster and overwrites the removed bytes immediately.
    pub fn pop(&mut self) {
        if let Some((at, _)) = holonomy_eye::Clusters::new(self).last() {
            self.storage[at..self.len].zeroize();
            self.len = at;
        }
    }
}

impl std::ops::Deref for SecretInput {
    type Target = str;
    fn deref(&self) -> &str {
        std::str::from_utf8(&self.storage[..self.len])
            .expect("SecretInput accepts only valid UTF-8")
    }
}

impl Zeroize for SecretInput {
    fn zeroize(&mut self) {
        self.storage.as_mut_slice().zeroize();
        self.len = 0;
    }
}

/// Maps an exact UTF-16 range to UTF-8 byte boundaries, refusing split surrogate pairs and
/// out-of-bounds OS requests rather than slicing a code point or guessing a document offset.
pub fn utf16_range_bytes(
    text: &str,
    range: std::ops::Range<usize>,
) -> Option<std::ops::Range<usize>> {
    fn boundary(text: &str, target: usize) -> Option<usize> {
        let mut units = 0;
        for (byte, ch) in text.char_indices() {
            if units == target {
                return Some(byte);
            }
            units += ch.len_utf16();
            if units > target {
                return None;
            }
        }
        (units == target).then_some(text.len())
    }
    if range.start > range.end {
        return None;
    }
    Some(boundary(text, range.start)?..boundary(text, range.end)?)
}

fn require_durability() -> Result<()> {
    if cfg!(unix) {
        Ok(())
    } else {
        Err(std::io::Error::new(std::io::ErrorKind::Unsupported,
            "Encrypted editing is unavailable: this platform has no qualified durable storage backend. No file was created or modified.").into())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!(
                "holonomy-native-{}-{}.wavefunction",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            )))
        }
        fn session(&self) -> Session {
            Session::from_document(
                &self.0,
                Document::create_with(&self.0, b"native fixture passphrase", store::test_params())
                    .unwrap(),
            )
            .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn refused_close_preserves_pending_text_and_allows_retry() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        session.apply(Intent::Type("keep the unsaved text".into())).unwrap();
        let held = fixture.0.with_extension("held");
        std::fs::rename(&fixture.0, &held).unwrap();
        let refusal = session.prepare_close();
        std::fs::rename(&held, &fixture.0).unwrap();
        assert!(refusal.is_err());
        assert!(session.dirty());
        assert_eq!(session.editor.frame().unwrap().text(), "keep the unsaved text");
        session.prepare_close().unwrap();
        drop(session);
        let mut reopened = Session::open(&fixture.0, b"native fixture passphrase").unwrap();
        assert_eq!(reopened.editor.frame().unwrap().text(), "keep the unsaved text");
    }

    #[test]
    fn repeated_wipe_preserves_the_entire_verification_result() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        assert!(session.wipe());
        assert!(session.wipe());
        // Inject a failed original receipt (e.g. a canary failure): erased keys alone must not
        // upgrade that failure to success on a later idempotent call.
        session.emergency.wipe_verified = false;
        assert!(session.document.are_keys_zeroized());
        assert!(!session.wipe());
    }

    #[test]
    fn native_paint_cannot_accept_an_unapplied_hit_test_transform() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        assert!(session.apply(Intent::SetView(holonomy_eye::View::new(2_000, 96).unwrap())).is_err());
        assert_eq!(session.editor.view(), holonomy_eye::View::IDENTITY);
        assert!(session.apply(Intent::SetView(holonomy_eye::View::IDENTITY)).is_ok());
    }

    #[test]
    fn deadman_latches_before_further_access_and_cooperative_wipe_verifies_keys() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        session
            .apply(Intent::Type("uncommitted secret".into()))
            .unwrap();
        session.set_deadman(Duration::from_millis(10)).unwrap();
        std::thread::sleep(Duration::from_millis(30));
        assert!(session.tripped());
        assert!(session.activity().is_err());
        assert!(session.apply(Intent::Type("must refuse".into())).is_err());
        assert!(session.save().is_err());
        assert!(session.wipe());
        assert!(session.frame(2.).is_err());
    }

    #[test]
    fn encrypted_roundtrip_undo_search_history_and_vector_paint_share_one_core() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        session
            .apply(Intent::Type("Hello café — office\nsecond line".into()))
            .unwrap();
        assert!(session.dirty());
        let frame = session.frame(2.).unwrap();
        assert!(!frame.glyphs.is_empty());
        assert!(frame
            .glyphs
            .iter()
            .flat_map(|g| &g.contours)
            .flatten()
            .all(|&(x, y)| x.is_finite() && y.is_finite()));
        session.save().unwrap();
        assert!(!session.dirty());
        session.apply(Intent::Type("!".into())).unwrap();
        session.apply(Intent::Undo).unwrap();
        assert_eq!(
            session.editor.frame().unwrap().text(),
            "Hello café — office\nsecond line"
        );
        assert_eq!(session.find("café", 0).unwrap(), 1);
        assert_eq!(&**session.selected_text().unwrap(), "café");
        session.save().unwrap();
        let sequence = session.witnesses()[0];
        session.show_witness(sequence).unwrap();
        assert!(session
            .apply(Intent::Type("must not persist".into()))
            .is_err());
        assert!(session.in_history());
        session.show_current();
        assert!(!session.in_history());
        session.prepare_close().unwrap();
        drop(session);
        let mut reopened = Session::open(&fixture.0, b"native fixture passphrase").unwrap();
        assert_eq!(
            reopened.editor.frame().unwrap().text(),
            "Hello café — office\nsecond line"
        );
        reopened.verify().unwrap();
        assert!(reopened.wipe());
    }

    #[test]
    fn refused_open_or_create_never_overwrites_an_existing_document() {
        let fixture = Fixture::new();
        {
            let mut session = fixture.session();
            session.apply(Intent::Type("keep me".into())).unwrap();
            session.save().unwrap();
        }
        let before = std::fs::read(&fixture.0).unwrap();
        assert!(Session::open(&fixture.0, b"wrong passphrase").is_err());
        assert!(Session::create(&fixture.0, b"different passphrase").is_err());
        assert_eq!(std::fs::read(&fixture.0).unwrap(), before);
    }

    #[test]
    fn device_scale_changes_outline_detail_not_logical_layout_or_caret() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        session.apply(Intent::Type("office W café".into())).unwrap();
        let at_one = session.frame(1.).unwrap();
        let at_two = session.frame(2.).unwrap();
        assert_eq!(at_one.glyphs.len(), at_two.glyphs.len());
        assert_eq!(at_one.caret.unwrap().x, at_two.caret.unwrap().x);
        assert_eq!(at_one.caret.unwrap().y, at_two.caret.unwrap().y);
        for scale in [0., f32::NAN, f32::INFINITY, 1.25, 1.5, 1.75, 4.] {
            let output = session.frame(scale).unwrap();
            assert_eq!(at_one.glyphs.len(), output.glyphs.len());
        }
    }

    #[test]
    fn fixed_point_conversion_rejects_nonfinite_input() {
        assert_eq!(fixed(f32::NAN), Fixed::ZERO);
        assert_eq!(fixed(f32::INFINITY), Fixed::ZERO);
        assert_eq!(fixed(1.5), Fixed::from_64ths(96));
        assert_eq!(fixed(-1.5), Fixed::from_64ths(-96));
    }
}

#[cfg(test)]
mod input_tests {
    use super::*;
    #[test]
    fn secret_input_never_reallocates_and_clears_deleted_bytes() {
        let mut input = SecretInput::default();
        let ptr = input.storage.as_ptr();
        assert!(input.push_str("a😀é"));
        input.pop();
        assert_eq!(&*input, "a😀");
        assert!(input.storage[input.len..].iter().all(|&b| b == 0));
        input.pop();
        assert_eq!(&*input, "a");
        assert!(!input.push_str(&"x".repeat(4096)));
        assert_eq!(&*input, "a");
        assert_eq!(ptr, input.storage.as_ptr());
        input.zeroize();
        assert!(input.is_empty());
        assert!(input.storage.iter().all(|&b| b == 0));
    }
    #[test]
    fn composition_replacement_is_atomic_unicode_safe_and_zeroizes_its_tail() {
        let mut input = SecretInput::default();
        input.push_str("a😀bc");
        assert!(input.replace_utf16(1..3, "é"));
        assert_eq!(&*input, "aébc");
        assert!(input.storage[input.len..].iter().all(|&b| b == 0));
        assert!(!input.replace_utf16(0..0, &"z".repeat(4096)));
        assert_eq!(&*input, "aébc");
        input.zeroize();
        input.push_str("x👩‍💻");
        input.pop();
        assert_eq!(&*input, "x");
    }

    #[test]
    fn utf16_ranges_never_split_surrogates_or_accept_reversed_offsets() {
        assert_eq!(utf16_range_bytes("a😀é", 1..3), Some(1..5));
        assert_eq!(utf16_range_bytes("a😀é", 2..3), None);
        assert_eq!(utf16_range_bytes("a😀é", 3..4), Some(5..7));
        assert_eq!(utf16_range_bytes("a😀é", 0..5), None);
        assert_eq!(utf16_range_bytes("", 0..0), Some(0..0));
        let start = 3;
        let end = 1;
        assert_eq!(utf16_range_bytes("test", start..end), None);
    }
}

#[cfg(test)]
mod ui_smoke_fixture {
    use super::*;
    #[test]
    #[ignore = "fixture for the Xvfb native UI qualification job"]
    fn create() {
        let path = PathBuf::from(
            std::env::var_os("HOLONOMY_NATIVE_SMOKE_PATH").expect("explicit fixture path"),
        );
        let document =
            Document::create_with(&path, b"native smoke passphrase", store::test_params()).unwrap();
        let mut session = Session::from_document(&path, document).unwrap();
        session
            .apply(Intent::Type(
                "Native vector glyph specimen: office café 0123456789".into(),
            ))
            .unwrap();
        session.save().unwrap();
    }
    #[test]
    #[ignore = "checks the file saved by the Xvfb native UI qualification job"]
    fn verify() {
        let path = PathBuf::from(
            std::env::var_os("HOLONOMY_NATIVE_SMOKE_PATH").expect("explicit fixture path"),
        );
        let mut session = Session::open(&path, b"native smoke passphrase").unwrap();
        assert_eq!(
            session.editor.frame().unwrap().text(),
            "Saved by native UI smoke test"
        );
        session.verify().unwrap();
    }
}

#[cfg(all(test, not(unix)))]
mod unsupported_storage_tests {
    use super::*;
    #[test]
    fn unsupported_storage_is_refused_before_kdf_or_file_creation() {
        let path = std::env::temp_dir().join(format!(
            "holonomy-native-unsupported-{}.wavefunction",
            std::process::id()
        ));
        assert!(!path.exists());
        assert!(Session::create(&path, b"test passphrase").is_err());
        assert!(Session::open(&path, b"test passphrase").is_err());
        assert!(!path.exists());
    }
}
