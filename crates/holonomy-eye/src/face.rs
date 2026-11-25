//! A real face: OpenType metrics and `GDEF`/`GSUB`/`GPOS` shaping read straight out of a font
//! file (`docs/TO-APP.md` M3, `AUDIT OQ-24`).
//!
//! This module reads `head`, `hhea`, `hmtx`, `maxp`, `cmap` plus optional OpenType `GDEF`, `GSUB`,
//! and `GPOS` layout tables (parsed by `opentype.rs` with zero external dependencies and zero
//! `unsafe`), so proportional advances, kerning (`PairPos`), ligatures/contextual forms (`GSUB`),
//! mark attachment (`MarkToBase`/`MarkToLigature`/`MarkToMark`), and Arabic/Syriac/N'Ko/Adlam
//! joining are real measurements rather than assumed per-character sums.
//!
//! # Why the file is parsed here and not by a library
//!
//! The workspace lockfile is committed and CI builds `--locked`, so adding a dependency like
//! `harfbuzz`/`rustybuzz` is a supply-chain decision, not a convenience. Reading the OpenType
//! tables directly in pure Rust preserves zero dependencies while satisfying [`Shaper`] for both
//! `Layout` and `PixelSurface`.
//!
//! Sizes are scaled exactly: `advance_units * size_milli * 64 / (units_per_em * 1000)` in `i64`,
//! rounded down, so a 2× size is within one sixty-fourth of a unit of twice the width rather than
//! drifting by a rounding error per glyph.

use crate::geom::Fixed;
use crate::opentype::{ShapedGlyph, ShapingTables};
use crate::shaper::Shaper;
use crate::style::Style;
use std::collections::BTreeMap;

/// Why a font file could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaceError {
    /// The file is shorter than the table directory it claims.
    Truncated,
    /// The file does not begin with a table directory an OpenType font would have.
    NotSfnt,
    /// A table the metrics contract needs is missing.
    MissingTable(&'static str),
    /// A table the metrics contract needs is malformed.
    BadTable(&'static str),
    /// The font's `units_per_em` is zero, so nothing can be scaled.
    NoScale,
}

impl core::fmt::Display for FaceError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Truncated => write!(formatter, "the font is truncated"),
            Self::NotSfnt => write!(formatter, "the file is not an OpenType font"),
            Self::MissingTable(table) => write!(formatter, "the font has no {table} table"),
            Self::BadTable(table) => write!(formatter, "the font's {table} table is malformed"),
            Self::NoScale => write!(formatter, "the font has no units_per_em"),
        }
    }
}

impl std::error::Error for FaceError {}

/// A face's metrics: advances per glyph, and the font's glyph for each character.
///
/// Parsed once and owned — no borrow of the file's bytes — so a face can outlive the buffer it was
/// read from. It implements [`Shaper`], which is the whole contract the Eye has with text.
#[derive(Debug, Clone)]
pub struct Face {
    units_per_em: u16,
    ascender: i16,
    descender: i16,
    advances: Vec<u16>,
    /// Character → glyph, sorted, so a lookup is a binary search.
    cmap: BTreeMap<u32, u16>,
    /// Optional OpenType `GDEF`/`GSUB`/`GPOS` shaping tables (`OQ-24`).
    shaping: ShapingTables,
}

impl Face {
    /// Reads a face's metrics and optional OpenType `GDEF`/`GSUB`/`GPOS` shaping tables from an
    /// OpenType or TrueType file's bytes.
    ///
    /// # Errors
    ///
    /// [`FaceError`] when the file is not an sfnt, is truncated, or is missing one of the five
    /// tables the metrics contract reads.
    pub fn parse(bytes: &[u8]) -> Result<Self, FaceError> {
        let tables = TableDirectory::parse(bytes)?;
        let head = tables.get(bytes, b"head")?;
        let units_per_em = u16::at(head, 18).ok_or(FaceError::BadTable("head"))?;
        if units_per_em == 0 {
            return Err(FaceError::NoScale);
        }
        let hhea = tables.get(bytes, b"hhea")?;
        let ascender = i16::at(hhea, 4).ok_or(FaceError::BadTable("hhea"))?;
        let descender = i16::at(hhea, 6).ok_or(FaceError::BadTable("hhea"))?;
        // `line_height` is ascender - descender, and every consumer (the height index, the reveal,
        // the scroll limit) is written for a positive line box. A `descender` above the ascender is
        // a font that would make it negative or zero, which release builds have no guard against;
        // refusing the font here is cheaper than defending the index downstream (D-55).
        if i32::from(ascender) - i32::from(descender) <= 0 {
            return Err(FaceError::BadTable("hhea"));
        }
        let metrics = usize::from(u16::at(hhea, 34).ok_or(FaceError::BadTable("hhea"))?);
        if metrics == 0 {
            return Err(FaceError::BadTable("hhea"));
        }
        let maxp = tables.get(bytes, b"maxp")?;
        let glyphs = usize::from(u16::at(maxp, 4).ok_or(FaceError::BadTable("maxp"))?);
        if glyphs == 0 || metrics > glyphs {
            return Err(FaceError::BadTable("maxp"));
        }
        let hmtx = tables.get(bytes, b"hmtx")?;
        let mut advances = Vec::with_capacity(glyphs);
        for glyph in 0..glyphs {
            // Beyond `numberOfHMetrics` a glyph repeats the last advance, per the spec.
            let index = glyph.min(metrics - 1);
            let advance = u16::at(hmtx, index * 4).ok_or(FaceError::BadTable("hmtx"))?;
            advances.push(advance);
        }
        let cmap = tables.get(bytes, b"cmap").and_then(read_cmap)?;
        let shaping = ShapingTables::parse(
            tables.get(bytes, b"GDEF").ok(),
            tables.get(bytes, b"GSUB").ok(),
            tables.get(bytes, b"GPOS").ok(),
        );
        Ok(Self {
            units_per_em,
            ascender,
            descender,
            advances,
            cmap,
            shaping,
        })
    }

    /// The font's design ascender in font units.
    #[must_use]
    pub fn ascender(&self) -> i16 {
        self.ascender
    }

    /// The font's design descender in font units.
    #[must_use]
    pub fn descender(&self) -> i16 {
        self.descender
    }

    /// The font's design units per em — the denominator every advance is scaled from.
    #[must_use]
    pub fn units_per_em(&self) -> u16 {
        self.units_per_em
    }

    /// How many glyphs the font has.
    #[must_use]
    pub fn glyph_count(&self) -> usize {
        self.advances.len()
    }

    /// How many characters the font maps to a glyph.
    #[must_use]
    pub fn mapped_characters(&self) -> usize {
        self.cmap.len()
    }

    /// The glyph `character` maps to, if the font has one.
    #[must_use]
    pub fn glyph(&self, character: char) -> Option<u16> {
        self.cmap.get(&u32::from(character)).copied()
    }

    /// The advance of one character, in font units, or the `.notdef` glyph's when it is unmapped.
    #[must_use]
    pub fn advance_units(&self, character: char) -> u16 {
        let glyph = self.glyph(character).unwrap_or(0);
        self.advances
            .get(usize::from(glyph))
            .copied()
            .unwrap_or_default()
    }

    /// Shapes a slice of grapheme clusters through this face's OpenType `GDEF`/`GSUB`/`GPOS`
    /// tables, returning positioned glyphs in font design units.
    #[must_use]
    pub fn shape(&self, clusters: &[&str]) -> Vec<ShapedGlyph> {
        self.shaping.shape(
            clusters,
            |ch| self.glyph(ch).unwrap_or(0),
            |gid| i32::from(self.advances.get(usize::from(gid)).copied().unwrap_or(0)),
        )
    }

    /// Scales `units` font units to logical units at `style`.
    #[must_use]
    pub fn scaled_signed(&self, units: i32, style: Style) -> Fixed {
        self.scaled(i64::from(units), style)
    }

    /// Scales `units` font units to logical units at `style`.
    pub(crate) fn scaled(&self, units: i64, style: Style) -> Fixed {
        let numerator = units * style.size_milli() * 64;
        let denominator = i64::from(self.units_per_em) * 1_000;
        Fixed::from_64ths(numerator / denominator)
    }

    /// A hard break has no width: it ends a line rather than occupying one.
    pub(crate) fn is_break(cluster: &str) -> bool {
        matches!(cluster, "\n" | "\r" | "\r\n")
    }
}

impl Shaper for Face {
    fn advance(&self, cluster: &str, style: Style) -> Fixed {
        if Self::is_break(cluster) {
            return Fixed::ZERO;
        }
        let mut chars = cluster.chars();
        let Some(first) = chars.next() else {
            return Fixed::ZERO;
        };
        if chars.next().is_none() && (first as u32) < 0x0600 {
            return self.scaled(i64::from(self.advance_units(first)), style);
        }
        let shaped = self.shape(&[cluster]);
        let units: i64 = shaped.iter().map(|g| i64::from(g.x_advance)).sum();
        self.scaled(units, style)
    }

    fn shape_clusters(&self, clusters: &[&str], style: Style, out: &mut Vec<Fixed>) {
        let mut fast_units = Vec::new();
        if self.shaping.shape_clusters_latin_fast(
            clusters,
            |ch| self.glyph(ch).unwrap_or(0),
            |gid| i32::from(self.advances.get(usize::from(gid)).copied().unwrap_or(0)),
            &mut fast_units,
        ) {
            out.clear();
            out.reserve(fast_units.len());
            for units in fast_units {
                out.push(self.scaled(i64::from(units), style));
            }
            return;
        }

        out.clear();
        out.resize(clusters.len(), Fixed::ZERO);
        if clusters.is_empty() {
            return;
        }
        let glyphs = self.shape(clusters);
        let mut per_cluster_units = vec![0i64; clusters.len()];
        for g in &glyphs {
            if g.cluster >= clusters.len() {
                continue;
            }
            if g.cluster_span <= 1 {
                per_cluster_units[g.cluster] += i64::from(g.x_advance);
            }
        }
        for (idx, &units) in per_cluster_units.iter().enumerate() {
            if !Self::is_break(clusters[idx]) {
                out[idx] = self.scaled(units, style);
            }
        }
        for g in &glyphs {
            if g.cluster < clusters.len() && g.cluster_span > 1 {
                let span = g.cluster_span.min(clusters.len() - g.cluster);
                let total = self.scaled(i64::from(g.x_advance), style).sixtyfourths();
                for j in 0..span {
                    let lo = total * (j as i64) / (span as i64);
                    let hi = total * (j as i64 + 1) / (span as i64);
                    if !Self::is_break(clusters[g.cluster + j]) {
                        out[g.cluster + j] += Fixed::from_64ths(hi - lo);
                    }
                }
            }
        }
    }

    fn line_height(&self, style: Style) -> Fixed {
        let total = i64::from(self.ascender) - i64::from(self.descender);
        // `parse` refuses a font whose `hhea` makes this non-positive (D-55), so the box is a
        // measurement rather than a rule that can run backwards.
        self.scaled(total, style)
    }

    fn ascent(&self, style: Style) -> Fixed {
        self.scaled(i64::from(self.ascender), style)
    }
}

/// The sfnt table directory: tag → (offset, length), validated against the file's own length.
pub(crate) struct TableDirectory(BTreeMap<[u8; 4], (usize, usize)>);

impl TableDirectory {
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, FaceError> {
        let version = u32::at(bytes, 0).ok_or(FaceError::Truncated)?;
        // TrueType (`0x00010000`), CFF (`OTTO`), and Apple's `true`/`ttcf` are all sfnt containers.
        let sfnt =
            version == 0x0001_0000 || bytes.starts_with(b"OTTO") || bytes.starts_with(b"true");
        if !sfnt {
            return Err(FaceError::NotSfnt);
        }
        let count = usize::from(u16::at(bytes, 4).ok_or(FaceError::Truncated)?);
        let mut tables = BTreeMap::new();
        for index in 0..count {
            let record = 12 + index * 16;
            let tag = bytes.get(record..record + 4).ok_or(FaceError::Truncated)?;
            let mut name = [0u8; 4];
            name.copy_from_slice(tag);
            let offset = usize::try_from(u32::at(bytes, record + 8).ok_or(FaceError::Truncated)?)
                .map_err(|_| FaceError::Truncated)?;
            let length = usize::try_from(u32::at(bytes, record + 12).ok_or(FaceError::Truncated)?)
                .map_err(|_| FaceError::Truncated)?;
            let end = offset.checked_add(length).ok_or(FaceError::Truncated)?;
            if end > bytes.len() {
                return Err(FaceError::Truncated);
            }
            tables.insert(name, (offset, length));
        }
        Ok(Self(tables))
    }

    pub(crate) fn get<'a>(&self, bytes: &'a [u8], tag: &[u8; 4]) -> Result<&'a [u8], FaceError> {
        let missing = match tag {
            b"head" => "head",
            b"hhea" => "hhea",
            b"hmtx" => "hmtx",
            b"maxp" => "maxp",
            b"loca" => "loca",
            b"glyf" => "glyf",
            _ => "cmap",
        };
        let (offset, length) = *self.0.get(tag).ok_or(FaceError::MissingTable(missing))?;
        bytes
            .get(offset..offset + length)
            .ok_or(FaceError::MissingTable(missing))
    }
}

/// Reads `cmap` into character → glyph, preferring a full Unicode subtable over a BMP one.
fn read_cmap(cmap: &[u8]) -> Result<BTreeMap<u32, u16>, FaceError> {
    let count = usize::from(u16::at(cmap, 2).ok_or(FaceError::BadTable("cmap"))?);
    let mut bmp: Option<usize> = None;
    let mut full: Option<usize> = None;
    for index in 0..count {
        let record = 4 + index * 8;
        let platform = u16::at(cmap, record).ok_or(FaceError::BadTable("cmap"))?;
        let encoding = u16::at(cmap, record + 2).ok_or(FaceError::BadTable("cmap"))?;
        let offset = usize::try_from(u32::at(cmap, record + 4).ok_or(FaceError::BadTable("cmap"))?)
            .map_err(|_| FaceError::BadTable("cmap"))?;
        let format = u16::at(cmap, offset).ok_or(FaceError::BadTable("cmap"))?;
        // (3, 10) is full Unicode, (3, 1) and (0, x) cover the BMP. Nothing else is read.
        if format == 12 && (platform == 3 && encoding == 10 || platform == 0) {
            full.get_or_insert(offset);
        } else if format == 4 && (platform == 3 && encoding == 1 || platform == 0) {
            bmp.get_or_insert(offset);
        }
    }
    if let Some(offset) = full {
        return read_cmap_format_12(cmap, offset);
    }
    if let Some(offset) = bmp {
        return read_cmap_format_4(cmap, offset);
    }
    Err(FaceError::BadTable("cmap"))
}

fn read_cmap_format_4(cmap: &[u8], offset: usize) -> Result<BTreeMap<u32, u16>, FaceError> {
    let bad = || FaceError::BadTable("cmap");
    let segments = usize::from(u16::at(cmap, offset + 6).ok_or_else(bad)?) / 2;
    let ends = offset + 14;
    let starts = ends + segments * 2 + 2;
    let deltas = starts + segments * 2;
    let ranges = deltas + segments * 2;
    let mut map = BTreeMap::new();
    for segment in 0..segments {
        let end = u16::at(cmap, ends + segment * 2).ok_or_else(bad)?;
        let start = u16::at(cmap, starts + segment * 2).ok_or_else(bad)?;
        let delta = i16::at(cmap, deltas + segment * 2).ok_or_else(bad)?;
        let range = u16::at(cmap, ranges + segment * 2).ok_or_else(bad)?;
        if start > end {
            continue;
        }
        for code in u32::from(start)..=u32::from(end) {
            let glyph = if range == 0 {
                (code as i32 + i32::from(delta)) as u16
            } else {
                // `idRangeOffset` is a byte offset from its own slot into `glyphIdArray`.
                let slot = ranges + segment * 2;
                let at = slot
                    + usize::from(range)
                    + usize::try_from((code - u32::from(start)) * 2).map_err(|_| bad())?;
                let index = u16::at(cmap, at).ok_or_else(bad)?;
                if index == 0 {
                    0
                } else {
                    (i32::from(index) + i32::from(delta)) as u16
                }
            };
            if glyph != 0 {
                map.insert(code, glyph);
            }
        }
    }
    Ok(map)
}

fn read_cmap_format_12(cmap: &[u8], offset: usize) -> Result<BTreeMap<u32, u16>, FaceError> {
    let bad = || FaceError::BadTable("cmap");
    let groups = usize::try_from(u32::at(cmap, offset + 12).ok_or_else(bad)?).map_err(|_| bad())?;
    let mut map = BTreeMap::new();
    for group in 0..groups {
        let at = offset + 16 + group * 12;
        let start = u32::at(cmap, at).ok_or_else(bad)?;
        let end = u32::at(cmap, at + 4).ok_or_else(bad)?;
        let first = u32::at(cmap, at + 8).ok_or_else(bad)?;
        if start > end || end - start > 0xFFFF {
            continue;
        }
        for code in start..=end {
            let glyph = first + (code - start);
            if let Ok(glyph) = u16::try_from(glyph) {
                map.insert(code, glyph);
            }
        }
    }
    Ok(map)
}

/// A big-endian read that returns `None` rather than panicking on a short table.
pub(crate) trait ReadAt {
    fn at(bytes: &[u8], offset: usize) -> Option<Self>
    where
        Self: Sized;
}

impl ReadAt for u16 {
    fn at(bytes: &[u8], offset: usize) -> Option<Self> {
        let slice = bytes.get(offset..offset.checked_add(2)?)?;
        Some(Self::from_be_bytes([slice[0], slice[1]]))
    }
}

impl ReadAt for i16 {
    fn at(bytes: &[u8], offset: usize) -> Option<Self> {
        Some(Self::from_be_bytes(u16::at(bytes, offset)?.to_be_bytes()))
    }
}

impl ReadAt for u32 {
    fn at(bytes: &[u8], offset: usize) -> Option<Self> {
        let slice = bytes.get(offset..offset.checked_add(4)?)?;
        Some(Self::from_be_bytes([
            slice[0], slice[1], slice[2], slice[3],
        ]))
    }
}
