//! Native frontend session boundary: existing encrypted storage and editing semantics, with
//! document glyphs exported as vector geometry rather than framework-owned plaintext strings.

use std::path::{Path, PathBuf};

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
        Self::from_document(path, Document::open(path, passphrase)?)
    }

    /// Creates a new document, refusing an existing destination.
    pub fn create(path: &Path, passphrase: &[u8]) -> Result<Self> {
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
        self.editor.flush()?;
        Ok(())
    }

    /// Flushes before leaving, so a refusal keeps the session available for recovery.
    pub fn prepare_close(&mut self) -> Result<()> {
        self.save()
    }

    /// Resizes both views using logical coordinates, independently of the monitor scale.
    pub fn resize(&mut self, width: f32, height: f32) {
        self.width = fixed(width.max(40.));
        self.height = fixed(height.max(40.));
        self.editor.resize(self.width, self.height);
        if let Some(history) = &mut self.history {
            history.resize(self.width, self.height);
        }
    }

    /// Finds and selects one streaming search result without collecting every match.
    pub fn find(&mut self, needle: &str, index: u64) -> Result<u64> {
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
        self.document.verify()?;
        Ok(())
    }

    /// Volatile-wipes resident editor buffers and unlocked storage keys. The caller must also
    /// discard the GPU scene and its own input buffers; this does not certify GPU memory erasure.
    pub fn wipe(&mut self) -> bool {
        self.editor.wipe_all_in_place();
        if let Some(history) = &mut self.history {
            history.wipe_all_in_place();
        }
        let arenas = holonomy_seal::SecretArenas::new();
        // SAFETY: the UI thread owns the session exclusively and performs the wipe synchronously,
        // with no document/key operations between registration and completion.
        unsafe {
            self.document.arm_reaper(&arenas);
        }
        arenas.wipe();
        self.document.are_keys_zeroized()
    }

    /// Computes toolbar formatting from the core selection, never from a second document model.
    pub fn toolbar(&self) -> holonomy_text::ToolbarState {
        self.editor.toolbar_state()
    }

    /// Copies the explicitly selected plaintext for an explicit clipboard operation only.
    /// The caller owns clipboard disclosure and cleanup policy.
    pub fn selected_text(&mut self) -> Result<zeroize::Zeroizing<String>> {
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
        for line in frame.layout().lines() {
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
                let baseline = line.top().to_f32() + self.fonts.ascent(style).to_f32();
                let mut last_cluster = usize::MAX;
                let mut pen = 0.;
                for (variant, glyph) in self.fonts.shape_run(&slices, style) {
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
                        + self
                            .fonts
                            .scale_units(variant, glyph.x_offset, style)
                            .to_f32();
                    let y = baseline
                        - self
                            .fonts
                            .scale_units(variant, glyph.y_offset, style)
                            .to_f32();
                    if let Some(mut outline) =
                        self.fonts
                            .vector_glyph(variant, glyph.glyph_id, style, device_scale)?
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
                    pen += self
                        .fonts
                        .scale_units(variant, glyph.x_advance, style)
                        .to_f32();
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
                        y: baseline - self.fonts.ascent(style).to_f32() / 3.,
                        width,
                        height: 1.,
                    });
                }
                start = end;
            }
        }
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

#[cfg(test)]
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
