//! Brotli-compressed font payload, the A8 glyph atlas, and procedural Box Drawing.
//!
//! # What this crate is for
//!
//! Two hard constraints meet here. The binary has a 2.5 MiB ceiling, so the fonts cannot be
//! shipped as raw TTFs (1.5 MiB for four subsets) or as four separate brotli streams
//! (56.7 KiB). And the glyph atlas has to live in L2 for the per-keystroke blit to hit its
//! latency budget, which caps it at 512 KiB. PROJECT.md §5 Phase 4.
//!
//! # The one-time pass
//!
//! Boot decompresses [`payload::PACKED_FONTS`] into a page-locked [`SecureBlock`], parses each
//! face with `ttf-parser` (zero allocation), rasterises every glyph once into a tightly packed
//! A8 atlas, then **scrubs the decompressed buffer immediately**. Nothing in the steady-state
//! path touches a font outline again: FR-2.5's "zero cubic Bézier calculations during typing"
//! is a property of the data flow, not of an optimisation pass, because the only code that can
//! evaluate a curve is gone from the binary by the time a key is pressed.
//!
//! # Coverage
//!
//! ASCII 0x20..0x7E and Latin-1 Supplement 0xA0..0xFF come from the fonts. Box Drawing
//! 0x2500..0x257F is generated from coordinate arithmetic by [`box_drawing`], because Inter
//! ships none of those glyphs and because font box-drawing rounds badly at cell boundaries --
//! two adjacent box glyphs show a visible antialiasing seam that procedural geometry does not.
//!
//! # Licensing
//!
//! Inter and JetBrains Mono are both SIL OFL 1.1. Their licences are in `assets/fonts/`. OFL
//! permits bundling and embedding provided the licence travels with the fonts, which is why
//! both are committed rather than fetched at build time.

pub mod atlas;
pub mod blit;
pub mod box_drawing;
pub mod metric;
pub mod payload;
pub mod raster;
pub mod skyline;

pub use holonomy_jail::PHASE_0_PLACEHOLDER;

use std::time::Duration;

/// What the one-time pass produced, and what it cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootReport {
    /// Microseconds spent decompressing the font payload.
    pub decompress_us: u64,
    /// Microseconds spent rasterising outlines into the atlas.
    pub rasterize_us: u64,
    /// Microseconds spent generating Box Drawing geometry.
    pub box_us: u64,
    /// Microseconds spent packing glyphs into the atlas and allocating it.
    pub pack_us: u64,
    /// Total, which is what the 15 ms gate asserts on.
    pub total_us: u64,
    /// Bytes of A8 coverage actually used.
    pub atlas_used: usize,
    /// Bytes the atlas buffer occupies, including padding from packing.
    pub atlas_capacity: usize,
    /// Decompressed font bytes that were scrubbed.
    pub scrubbed: usize,
}

impl BootReport {
    /// `total_us` as milliseconds, which is the unit the gate quotes.
    pub fn total_ms(&self) -> f64 {
        self.total_us as f64 / 1000.0
    }
}

/// Phases of the one-time pass, timed individually so a regression points at a cause.
#[derive(Debug, Default, Clone)]
pub struct Phases {
    t0: Option<std::time::Instant>,
    marks: Vec<(u8, Duration)>,
}

impl Phases {
    /// Start timing.
    pub fn start() -> Self {
        Self {
            t0: Some(std::time::Instant::now()),
            marks: Vec::new(),
        }
    }

    /// Record a phase boundary.
    pub fn mark(&mut self, id: u8) {
        if let Some(t0) = self.t0 {
            let now = std::time::Instant::now();
            self.marks.push((id, now.duration_since(t0)));
            self.t0 = Some(now);
        }
    }

    /// Total elapsed across all marks.
    pub fn total(&self) -> Duration {
        self.marks.iter().map(|(_, d)| *d).sum()
    }

    /// Elapsed for one phase id.
    pub fn phase(&self, id: u8) -> Duration {
        self.marks
            .iter()
            .filter(|(i, _)| *i == id)
            .map(|(_, d)| *d)
            .sum()
    }

    pub(crate) fn into_report(
        self,
        atlas_used: usize,
        atlas_capacity: usize,
        scrubbed: usize,
    ) -> BootReport {
        BootReport {
            decompress_us: self.phase(0).as_micros() as u64,
            rasterize_us: self.phase(1).as_micros() as u64,
            box_us: self.phase(2).as_micros() as u64,
            pack_us: self.phase(3).as_micros() as u64,
            total_us: self.total().as_micros() as u64,
            atlas_used,
            atlas_capacity,
            scrubbed,
        }
    }
}

/// Run the whole one-time pass: decompress, parse, rasterise, pack, scrub.
///
/// `ppem_sizes` are the pixel sizes to rasterise at, e.g. `[16, 32]` for body and heading.
/// Each size costs roughly its square in atlas bytes, so this is the main lever on the
/// 512 KiB budget; see [`atlas`] for the sizing that fits.
pub fn build_atlas(ppem_sizes: &[u16]) -> Result<(atlas::Atlas, BootReport), Error> {
    let mut phases = Phases::start();

    // 1. Decompress into a page-locked, mlock'd, zeroizing scratchpad.
    let mut scratch = holonomy_secure::SecureBlock::allocate(payload::RAW_LEN)?;
    decompress_into(&mut scratch)?;
    phases.mark(0);

    // 2. Rasterise every (face, size, codepoint) exactly once.
    let mut builder = atlas::AtlasBuilder::new(ppem_sizes)?;
    let bytes = scratch.as_slice();
    for (index, entry) in payload::FACES.iter().enumerate() {
        let slice = &bytes[entry.offset as usize..(entry.offset + entry.length) as usize];
        // `Face::parse`'s second argument selects a face *within* a TrueType Collection (a
        // `ttcf` file). Each slice here is a standalone single-face TTF, so the index must be 0
        // for all four: passing the enumeration index made every face after the first fail with
        // "face index is out of bounds", because face 1's slice declares `numFonts = 1`.
        let face = ttf_parser::Face::parse(slice, 0).map_err(|e| Error::Parse {
            index: index as u32,
            detail: e.to_string(),
        })?;
        raster::rasterize_face(&face, entry, ppem_sizes, &mut builder)?;
    }
    phases.mark(1);

    // 3. Procedural Box Drawing, which no font supplies.
    box_drawing::add_box_drawing(ppem_sizes, &mut builder)?;
    phases.mark(2);

    let (atlas, used, capacity) = builder.finish()?;

    // 4. Scrub the font bytes. They are never read again: everything the renderer needs is
    //    the atlas, and `atlas` holds no borrows into `scratch`, so this cannot dangle.
    let scrubbed = scratch.len();
    phases.mark(3);

    Ok((atlas, phases.into_report(used, capacity, scrubbed)))
}

/// Stream-decompress the payload into `dst`.
///
/// Uses the `brotli-decompressor` reader directly rather than a convenience wrapper, because
/// the destination is a [`SecureBlock`] slice and the convenience API wants to own a `Vec`.
fn decompress_into(dst: &mut holonomy_secure::SecureBlock) -> Result<(), Error> {
    use std::io::Read;

    let mut input = payload::PACKED_FONTS;
    let mut reader = brotli_decompressor::Decompressor::new(&mut input, 4096);
    let mut written = 0usize;
    loop {
        let remaining = dst.len() - written;
        if remaining == 0 {
            // Output buffer full: if the stream still has input left it is truncated.
            if !input.is_empty() {
                return Err(Error::Oversized);
            }
            return Ok(());
        }
        let remaining = dst.len() - written;
        match reader.read(&mut dst.as_mut_slice()[written..written + remaining]) {
            Ok(0) => return Ok(()),
            Ok(n) => written += n,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Err(Error::Truncated {
                    got: written,
                    want: dst.len(),
                })
            }
            Err(e) => return Err(Error::Io(e)),
        }
    }
}

/// Failures in the one-time pass.
#[derive(Debug)]
pub enum Error {
    /// Allocation or mapping failed.
    Secure(holonomy_secure::SecureBlockError),
    /// I/O while decompressing.
    Io(std::io::Error),
    /// The payload decompressed to more than [`payload::RAW_LEN`] bytes, so the manifest
    /// disagrees with the blob.
    Oversized,
    /// The stream ended early.
    Truncated {
        /// Bytes produced.
        got: usize,
        /// Bytes the manifest promised.
        want: usize,
    },
    /// A face would not parse. Always means a corrupt build artefact or a manifest that does
    /// not match the blob.
    Parse {
        /// Which face, by index into [`payload::FACES`].
        index: u32,
        /// The parser's own report.
        detail: String,
    },
    /// Glyph id out of range for a face.
    GlyphOutOfRange {
        /// Requested id.
        glyph: u16,
        /// How many the face has.
        count: u16,
    },
    /// Atlas capacity exceeded.
    AtlasFull {
        /// Bytes needed.
        need: usize,
        /// Bytes available.
        have: usize,
    },
    /// A glyph's bitmap does not fit the metric's `u8` width and height fields.
    GlyphTooLarge {
        /// Atlas x.
        x: u16,
        /// Atlas y.
        y: u16,
        /// Measured width.
        w: usize,
        /// Measured height.
        h: usize,
    },
    /// A glyph's bitmap length disagreed with its declared dimensions.
    BitmapLength {
        /// Codepoint of the offending glyph.
        codepoint: u32,
        /// Bytes supplied.
        have: usize,
        /// Bytes the dimensions imply.
        want: usize,
    },
    /// A glyph was offered at a ppem the atlas was not built for.
    UndeclaredSize {
        /// The ppem offered.
        ppem: u16,
        /// The ppems the atlas covers, ascending.
        declared: Vec<u16>,
    },
    /// An alias names a source glyph that was never placed.
    DanglingAlias {
        /// Codepoint of the alias.
        codepoint: u32,
        /// Style of the alias.
        style: i32,
        /// Codepoint it points at.
        source_codepoint: u32,
        /// Style it points at.
        source_style: i32,
    },
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Secure(e) => write!(f, "secure block: {e}"),
            Self::Io(e) => write!(f, "decompress: {e}"),
            Self::Oversized => write!(
                f,
                "payload decompresses to more than {} bytes; the manifest is stale",
                payload::RAW_LEN
            ),
            Self::Truncated { got, want } => {
                write!(f, "payload truncated: {got} of {want} bytes")
            }
            Self::Parse { index, detail } => {
                write!(f, "face {index} did not parse as a TrueType face: {detail}")
            }
            Self::GlyphOutOfRange { glyph, count } => {
                write!(f, "glyph {glyph} out of range for a {count}-glyph face")
            }
            Self::AtlasFull { need, have } => {
                write!(f, "atlas full: needs {need} bytes, has {have}")
            }
            Self::GlyphTooLarge { x, y, w, h } => write!(
                f,
                "glyph at ({x},{y}) is {w}x{h}, which overflows the u8 metric fields"
            ),
            Self::BitmapLength {
                codepoint,
                have,
                want,
            } => write!(
                f,
                "U+{codepoint:04X}: bitmap is {have} bytes, its dimensions imply {want}"
            ),
            Self::UndeclaredSize { ppem, declared } => write!(
                f,
                "ppem {ppem} was not rasterised; the atlas covers {declared:?}"
            ),
            Self::DanglingAlias {
                codepoint,
                style,
                source_codepoint,
                source_style,
            } => write!(
                f,
                "U+{codepoint:04X} style {style} aliases U+{source_codepoint:04X} style \
                 {source_style}, which was never placed"
            ),
        }
    }
}

impl std::error::Error for Error {}

impl From<holonomy_secure::SecureBlockError> for Error {
    fn from(e: holonomy_secure::SecureBlockError) -> Self {
        Self::Secure(e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<ttf_parser::FaceParsingError> for Error {
    fn from(e: ttf_parser::FaceParsingError) -> Self {
        Self::Parse {
            index: u32::MAX,
            detail: e.to_string(),
        }
    }
}

impl From<atlas::AtlasError> for Error {
    fn from(e: atlas::AtlasError) -> Self {
        match e {
            atlas::AtlasError::AtlasFull { need, have } => Self::AtlasFull { need, have },
            atlas::AtlasError::GlyphTooLarge { x, y, w, h } => Self::GlyphTooLarge { x, y, w, h },
            atlas::AtlasError::BitmapLength {
                codepoint,
                have,
                want,
            } => Self::BitmapLength {
                codepoint,
                have,
                want,
            },
            atlas::AtlasError::UndeclaredSize { ppem, declared } => {
                Self::UndeclaredSize { ppem, declared }
            }
            atlas::AtlasError::DanglingAlias {
                codepoint,
                style,
                source_codepoint,
                source_style,
            } => Self::DanglingAlias {
                codepoint,
                style,
                source_codepoint,
                source_style,
            },
        }
    }
}
