//! Pure-Rust OpenType `GDEF`, `GSUB`, and `GPOS` shaping engine (`AUDIT OQ-24`).
//!
//! Reads OpenType layout tables directly out of SFNT font bytes with zero external dependencies and
//! zero `unsafe`:
//! - **`GDEF`**: `GlyphClassDef` (Base / Ligature / Mark / Component), `MarkAttachClassDef`, and
//!   `MarkGlyphSetsDef` for lookup-flag filtering.
//! - **`GSUB`**: `ScriptList`, `FeatureList`, and `LookupList` across Lookup Types `1` (Single),
//!   `2` (Multiple), `3` (Alternate), `4` (Ligature), `5` (Contextual), `6` (Chaining Contextual),
//!   and `7` (Extension).
//! - **`GPOS`**: Lookup Types `1` (Single), `2` (PairPos Format 1 & Format 2), `4` (MarkToBase),
//!   `5` (MarkToLigature), `6` (MarkToMark), and `9` (Extension).
//! - **Script & Joining Engine**: Selects script tags (`arab`, `syrc`, `nko `, `adlm`, `hebr`,
//!   `deva`, `latn`, `DFLT`) and computes Unicode Arabic/Syriac/N'Ko/Adlam contextual joining forms
//!   (`isol`, `init`, `medi`, `fina`) while respecting `ZWJ`/`ZWNJ` and transparent marks.

use std::collections::BTreeMap;

use crate::face::ReadAt;

const FLAG_RIGHT_TO_LEFT: u16 = 0x0001;
const FLAG_IGNORE_BASE: u16 = 0x0002;
const FLAG_IGNORE_LIGATURES: u16 = 0x0004;
const FLAG_IGNORE_MARKS: u16 = 0x0008;
const FLAG_USE_MARK_FILTERING_SET: u16 = 0x0010;
const FLAG_MARK_ATTACHMENT_MASK: u16 = 0xFF00;

const MAX_OUTPUT_GLYPHS: usize = 4_096;
const MAX_RECURSE_DEPTH: usize = 8;

/// Feature bitmask attached to each glyph during shaping (controls Arabic/script joining).
pub(crate) const MASK_GLOBAL: u32 = 1 << 0;
pub(crate) const MASK_ISOL: u32 = 1 << 1;
pub(crate) const MASK_INIT: u32 = 1 << 2;
pub(crate) const MASK_MEDI: u32 = 1 << 3;
pub(crate) const MASK_FINA: u32 = 1 << 4;

/// One shaped glyph emitted by [`ShapingTables::shape`] in font design units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShapedGlyph {
    /// OpenType glyph index (`0` = `.notdef`).
    pub glyph_id: u16,
    /// First logical cluster index (`0..n_clusters`) covered by this glyph.
    pub cluster: usize,
    /// Number of consecutive logical clusters merged into this glyph (`1` normally, `>= 2` for
    /// ligatures such as `fi` or `->`).
    pub cluster_span: usize,
    /// Horizontal advance in font design units.
    pub x_advance: i32,
    /// Horizontal placement offset from pen position in font design units (positive = right).
    pub x_offset: i32,
    /// Vertical placement offset from baseline in font design units (positive = upward).
    pub y_offset: i32,
}

/// One glyph slot inside the mutable shaping buffer.
#[derive(Debug, Clone, Copy)]
pub(crate) struct GlyphSlot {
    pub gid: u16,
    pub cluster: usize,
    pub cluster_span: usize,
    pub mask: u32,
    pub x_advance: i32,
    pub x_offset: i32,
    pub y_offset: i32,
    /// Index in the buffer of the base/ligature/mark this mark attached to (`GPOS` Type 4/5/6).
    pub attach_to: Option<usize>,
    /// Ligature component index (`0`-based) for `MarkToLigature` attachment.
    pub lig_comp: u16,
    /// Ligature instance id so stacked marks distinguish distinct ligatures.
    pub lig_id: u16,
}

/// 1024-bit modulo glyph filter (`gid & 1023`) for O(1) lookup skipping with zero false negatives.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct CovMask([u64; 16]);

impl CovMask {
    const fn empty() -> Self {
        Self([0; 16])
    }

    const fn all() -> Self {
        Self([u64::MAX; 16])
    }

    #[inline]
    fn insert(&mut self, gid: u16) {
        let word = usize::from(gid >> 6) & 15;
        let bit = gid & 63;
        self.0[word] |= 1u64 << bit;
    }

    #[inline]
    fn union_with(&mut self, other: &Self) {
        for i in 0..16 {
            self.0[i] |= other.0[i];
        }
    }

    #[inline]
    fn contains(&self, gid: u16) -> bool {
        let word = usize::from(gid >> 6) & 15;
        let bit = gid & 63;
        (self.0[word] & (1u64 << bit)) != 0
    }

    #[inline]
    fn count_ones(&self) -> usize {
        self.0.iter().map(|w| w.count_ones() as usize).sum()
    }

    #[inline]
    fn intersects(&self, other: &Self) -> bool {
        let mut any = 0u64;
        for i in 0..16 {
            any |= self.0[i] & other.0[i];
        }
        any != 0
    }
}

/// Parsed OpenType `GDEF`, `GSUB`, and `GPOS` layout tables owned by a [`crate::Face`].
#[derive(Debug, Clone, Default)]
pub(crate) struct ShapingTables {
    gdef: GdefData,
    gsub: Option<LayoutTable>,
    gpos: Option<LayoutTable>,
    default_gsub: Vec<u16>,
    default_gpos: Vec<u16>,
    default_gsub_mask: CovMask,
    default_nonpair_gpos_mask: CovMask,
}

impl ShapingTables {
    pub(crate) fn parse(
        gdef_bytes: Option<&[u8]>,
        gsub_bytes: Option<&[u8]>,
        gpos_bytes: Option<&[u8]>,
    ) -> Self {
        let gdef = gdef_bytes.map(GdefData::parse).unwrap_or_default();
        let gsub = gsub_bytes.and_then(|b| LayoutTable::parse(b, true));
        let gpos = gpos_bytes.and_then(|b| LayoutTable::parse(b, false));

        let default_gsub = gsub
            .as_ref()
            .map(|t| {
                t.collect_lookups(
                    *b"latn",
                    &[*b"ccmp", *b"rlig", *b"liga", *b"clig", *b"calt"],
                )
            })
            .unwrap_or_default();
        let default_gpos = gpos
            .as_ref()
            .map(|t| t.collect_lookups(*b"latn", &[*b"kern", *b"mark", *b"mkmk", *b"curs"]))
            .unwrap_or_default();

        let mut default_gsub_mask = CovMask::empty();
        if let Some(ref t) = gsub {
            for &idx in &default_gsub {
                if let Some(l) = t.lookups.get(usize::from(idx)) {
                    default_gsub_mask.union_with(&l.trigger_mask);
                }
            }
        }
        let mut default_nonpair_gpos_mask = CovMask::empty();
        if let Some(ref t) = gpos {
            for &idx in &default_gpos {
                if let Some(l) = t.lookups.get(usize::from(idx)) {
                    if l.subtables
                        .iter()
                        .any(|s| !matches!(s, Subtable::GposPair(_)))
                    {
                        default_nonpair_gpos_mask.union_with(&l.trigger_mask);
                    }
                }
            }
        }

        Self {
            gdef,
            gsub,
            gpos,
            default_gsub,
            default_gpos,
            default_gsub_mask,
            default_nonpair_gpos_mask,
        }
    }

    /// Fast O(N) zero-allocation shaping path for pure single-codepoint basic-Latin runs when no
    /// default `GSUB` rule or non-pair `GPOS` lookup can match any glyph in `clusters`.
    ///
    /// Returns `false` if full `shape` is required; otherwise fills `out_units` with exact
    /// per-cluster design-unit advances including `GPOS` pair kerning.
    pub(crate) fn shape_clusters_latin_fast(
        &self,
        clusters: &[&str],
        glyph_for_char: impl Fn(char) -> u16,
        advance_for_glyph: impl Fn(u16) -> i32,
        out_units: &mut Vec<i32>,
    ) -> bool {
        out_units.clear();
        if clusters.is_empty() {
            return true;
        }
        out_units.reserve(clusters.len());
        let mut mask = CovMask::empty();
        let mut prev_gid: Option<u16> = None;
        for &cluster in clusters {
            let mut chars = cluster.chars();
            let Some(ch) = chars.next() else {
                out_units.push(0);
                continue;
            };
            if chars.next().is_some() || (ch as u32) >= 0x0250 {
                out_units.clear();
                return false;
            }
            if ch == '\n' || ch == '\r' {
                out_units.push(0);
                prev_gid = None;
                continue;
            }
            let gid = glyph_for_char(ch);
            mask.insert(gid);
            if mask.intersects(&self.default_gsub_mask)
                || mask.intersects(&self.default_nonpair_gpos_mask)
            {
                out_units.clear();
                return false;
            }
            if let Some(prev) = prev_gid {
                let kern = self.fast_pair_kern(prev, gid);
                if let Some(last) = out_units.last_mut() {
                    *last += kern;
                }
            }
            out_units.push(advance_for_glyph(gid));
            prev_gid = Some(gid);
        }
        true
    }

    /// Fast O(log N) pair kerning delta in font design units for two un-substituted glyphs under
    /// default `GPOS` `kern` lookups (ignoring marks).
    #[inline]
    pub(crate) fn fast_pair_kern(&self, left: u16, right: u16) -> i32 {
        let Some(ref gpos) = self.gpos else {
            return 0;
        };
        let mut total = 0i32;
        for &idx in &self.default_gpos {
            if let Some(lookup) = gpos.lookups.get(usize::from(idx)) {
                for sub in &lookup.subtables {
                    if let Subtable::GposPair(pair) = sub {
                        let (dx1, _) = pair.eval_x_advances(left, right);
                        total += dx1;
                    }
                }
            }
        }
        total
    }

    /// Runs full `GSUB` + `GPOS` shaping over `clusters`, calling `glyph_for_char` and
    /// `advance_for_glyph` from the owning [`crate::Face`].
    pub(crate) fn shape(
        &self,
        clusters: &[&str],
        glyph_for_char: impl Fn(char) -> u16,
        advance_for_glyph: impl Fn(u16) -> i32,
    ) -> Vec<ShapedGlyph> {
        if clusters.is_empty() {
            return Vec::new();
        }
        let mut chars: Vec<(char, usize)> = Vec::new();
        for (cidx, cluster) in clusters.iter().enumerate() {
            for ch in cluster.chars() {
                if ch == '\n' || ch == '\r' {
                    continue;
                }
                chars.push((ch, cidx));
            }
        }
        if chars.is_empty() {
            return Vec::new();
        }

        let script = detect_script(&chars);
        let joining_masks = if uses_arabic_joining(script) {
            compute_joining_masks(&chars)
        } else {
            vec![MASK_GLOBAL; chars.len()]
        };

        let mut buf: Vec<GlyphSlot> = Vec::with_capacity(chars.len());
        let mut buf_mask = CovMask::empty();
        for (idx, &(ch, cidx)) in chars.iter().enumerate() {
            let gid = glyph_for_char(ch);
            buf_mask.insert(gid);
            buf.push(GlyphSlot {
                gid,
                cluster: cidx,
                cluster_span: 1,
                mask: joining_masks[idx],
                x_advance: advance_for_glyph(gid),
                x_offset: 0,
                y_offset: 0,
                attach_to: None,
                lig_comp: 0,
                lig_id: 0,
            });
        }

        if let Some(ref gsub) = self.gsub {
            let mut gsub_mutated = false;
            if script == *b"latn" || script == *b"DFLT" {
                for &idx in &self.default_gsub {
                    if let Some(lookup) = gsub.lookups.get(usize::from(idx)) {
                        if !lookup.trigger_mask.intersects(&buf_mask) {
                            continue;
                        }
                        let gdef = &self.gdef;
                        let mask = MASK_GLOBAL;
                        if gsub.apply_gsub_lookup(idx, gdef, &mut buf, &buf_mask, mask, 0) {
                            gsub_mutated = true;
                            buf_mask = buffer_mask(&buf);
                        }
                    }
                }
            } else {
                let mut per_feature: Vec<([u8; 4], u32)> = vec![(*b"ccmp", MASK_GLOBAL)];
                if uses_arabic_joining(script) {
                    per_feature.extend_from_slice(&[
                        (*b"isol", MASK_ISOL),
                        (*b"init", MASK_INIT),
                        (*b"medi", MASK_MEDI),
                        (*b"fina", MASK_FINA),
                    ]);
                }
                per_feature.extend_from_slice(&[
                    (*b"rlig", MASK_GLOBAL),
                    (*b"liga", MASK_GLOBAL),
                    (*b"clig", MASK_GLOBAL),
                    (*b"calt", MASK_GLOBAL),
                ]);
                for (feat, mask) in per_feature {
                    let lookups = gsub.collect_lookups(script, &[feat]);
                    for idx in lookups {
                        if let Some(lookup) = gsub.lookups.get(usize::from(idx)) {
                            if !lookup.trigger_mask.intersects(&buf_mask) {
                                continue;
                            }
                            let gdef = &self.gdef;
                            if gsub.apply_gsub_lookup(idx, gdef, &mut buf, &buf_mask, mask, 0) {
                                gsub_mutated = true;
                                buf_mask = buffer_mask(&buf);
                            }
                        }
                    }
                }
            }
            if gsub_mutated {
                // Refresh base advances for glyphs created/replaced by `GSUB`.
                for slot in &mut buf {
                    slot.x_advance = advance_for_glyph(slot.gid);
                }
            }
        }

        if let Some(ref gpos) = self.gpos {
            let extra_lookups;
            let lookups = if script == *b"latn" || script == *b"DFLT" {
                &self.default_gpos
            } else {
                extra_lookups = gpos.collect_lookups(
                    script,
                    &[
                        *b"kern", *b"dist", *b"abvm", *b"blwm", *b"mark", *b"mkmk", *b"curs",
                    ],
                );
                &extra_lookups
            };
            let mut any_attached = false;
            for &idx in lookups {
                if let Some(lookup) = gpos.lookups.get(usize::from(idx)) {
                    if !lookup.trigger_mask.intersects(&buf_mask) {
                        continue;
                    }
                    if gpos.apply_gpos_lookup(idx, &self.gdef, &mut buf, &buf_mask, 0) {
                        any_attached = true;
                    }
                }
            }
            if any_attached {
                // Resolve chained attachment offsets (MarkToMark -> MarkToBase/Ligature).
                resolve_attachments(&mut buf);
            }
        }

        buf.into_iter()
            .map(|s| ShapedGlyph {
                glyph_id: s.gid,
                cluster: s.cluster,
                cluster_span: s.cluster_span,
                x_advance: s.x_advance,
                x_offset: s.x_offset,
                y_offset: s.y_offset,
            })
            .collect()
    }
}

#[inline]
fn buffer_mask(buf: &[GlyphSlot]) -> CovMask {
    let mut m = CovMask::empty();
    for s in buf {
        m.insert(s.gid);
    }
    m
}

fn resolve_attachments(buf: &mut [GlyphSlot]) {
    let n = buf.len();
    let local_offsets: Vec<(i32, i32)> = buf.iter().map(|s| (s.x_offset, s.y_offset)).collect();
    for i in 0..n {
        if buf[i].attach_to.is_none() {
            continue;
        }
        let mut cur = i;
        let mut dx = 0i32;
        let mut dy = 0i32;
        let mut adv_before_base = 0i32;
        for _ in 0..8 {
            let Some(parent) = buf[cur].attach_to else {
                break;
            };
            if parent >= n || parent == cur {
                break;
            }
            dx += local_offsets[cur].0;
            dy += local_offsets[cur].1;
            if parent < cur {
                // Subtract intervening advances between `parent` and `cur` so mark sits directly
                // on `parent` relative to `cur`'s pen position.
                for slot in buf.iter().take(cur).skip(parent) {
                    adv_before_base += slot.x_advance;
                }
            }
            cur = parent;
        }
        if cur != i && buf[cur].attach_to.is_none() {
            dx += local_offsets[cur].0;
            dy += local_offsets[cur].1;
        }
        buf[i].x_offset = dx - adv_before_base;
        buf[i].y_offset = dy;
    }
}

// ---------- Script & Arabic Contextual Joining ----------

fn detect_script(chars: &[(char, usize)]) -> [u8; 4] {
    for &(ch, _) in chars {
        match ch as u32 {
            0x0600..=0x06FF
            | 0x0750..=0x077F
            | 0x08A0..=0x08FF
            | 0xFB50..=0xFDFF
            | 0xFE70..=0xFEFF => return *b"arab",
            0x0700..=0x074F => return *b"syrc",
            0x07C0..=0x07FF => return *b"nko ",
            0x1E900..=0x1E95F => return *b"adlm",
            0x0590..=0x05FF => return *b"hebr",
            0x0900..=0x097F => return *b"deva",
            _ => {}
        }
    }
    *b"latn"
}

const fn uses_arabic_joining(script: [u8; 4]) -> bool {
    matches!(&script, b"arab" | b"syrc" | b"nko " | b"adlm")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JoiningType {
    NonJoining,
    Right,
    Dual,
    JoinCausing,
    Transparent,
}

fn joining_type(ch: char) -> JoiningType {
    let cp = ch as u32;
    if matches!(cp, 0x200D | 0x0640 | 0x07FA | 0x1E94B) {
        return JoiningType::JoinCausing;
    }
    if cp == 0x200C {
        return JoiningType::NonJoining;
    }
    // Combining marks across Arabic, Syriac, N'Ko, Adlam, and Unicode Mn/Cf ranges used inside words.
    if matches!(
        cp,
        0x0300..=0x036F
            | 0x0610..=0x061A
            | 0x064B..=0x065F
            | 0x0670
            | 0x06D6..=0x06DC
            | 0x06DF..=0x06E4
            | 0x06E7..=0x06E8
            | 0x06EA..=0x06ED
            | 0x0711
            | 0x0730..=0x074A
            | 0x07EB..=0x07F3
            | 0x08D3..=0x08E1
            | 0x08E3..=0x0902
            | 0x1E944..=0x1E94A
    ) {
        return JoiningType::Transparent;
    }
    // Right-joining Arabic/Syriac/N'Ko letters (Alef, Dal, Thal, Reh, Zain, Waw, Teh Marbuta, etc.).
    if matches!(
        cp,
        0x0622..=0x0625
            | 0x0627
            | 0x0629
            | 0x062F..=0x0632
            | 0x0648
            | 0x0671..=0x0673
            | 0x0675..=0x0677
            | 0x0688..=0x0699
            | 0x06C0
            | 0x06C3..=0x06CB
            | 0x06CD
            | 0x06CF
            | 0x06D2..=0x06D3
            | 0x06D5
            | 0x0710
            | 0x0715..=0x0719
            | 0x071E
            | 0x0728
            | 0x072A
            | 0x072C
    ) {
        return JoiningType::Right;
    }
    // Dual-joining letters across Arabic, Syriac, N'Ko, Adlam.
    if matches!(
        cp,
        0x0620
            | 0x0626
            | 0x0628
            | 0x062A..=0x062E
            | 0x0633..=0x063F
            | 0x0641..=0x0647
            | 0x0649..=0x064A
            | 0x066E..=0x066F
            | 0x0678..=0x0687
            | 0x069A..=0x06BF
            | 0x06C1..=0x06C2
            | 0x06CC
            | 0x06CE
            | 0x06D0..=0x06D1
            | 0x0712..=0x0714
            | 0x071A..=0x071D
            | 0x071F..=0x0727
            | 0x0729
            | 0x072B
            | 0x07CA..=0x07EA
            | 0x1E900..=0x1E943
    ) {
        return JoiningType::Dual;
    }
    JoiningType::NonJoining
}

fn compute_joining_masks(chars: &[(char, usize)]) -> Vec<u32> {
    let n = chars.len();
    let jtypes: Vec<JoiningType> = chars.iter().map(|&(ch, _)| joining_type(ch)).collect();
    let mut out = vec![MASK_GLOBAL; n];

    for i in 0..n {
        let jt = jtypes[i];
        if matches!(
            jt,
            JoiningType::NonJoining | JoiningType::JoinCausing | JoiningType::Transparent
        ) {
            continue;
        }
        let mut prev = None;
        for &pj in jtypes[..i].iter().rev() {
            if pj != JoiningType::Transparent {
                prev = Some(pj);
                break;
            }
        }
        let mut next = None;
        for &nj in &jtypes[i + 1..] {
            if nj != JoiningType::Transparent {
                next = Some(nj);
                break;
            }
        }

        let joins_prev = matches!(prev, Some(JoiningType::Dual | JoiningType::JoinCausing))
            && matches!(jt, JoiningType::Dual | JoiningType::Right);
        let joins_next = matches!(
            next,
            Some(JoiningType::Dual | JoiningType::Right | JoiningType::JoinCausing)
        ) && matches!(jt, JoiningType::Dual);

        let form = match (joins_prev, joins_next) {
            (false, false) => MASK_ISOL,
            (false, true) => MASK_INIT,
            (true, true) => MASK_MEDI,
            (true, false) => MASK_FINA,
        };
        out[i] |= form;
    }
    out
}

// ---------- GDEF ----------

#[derive(Debug, Clone, Default)]
struct GdefData {
    glyph_class: ClassDef,
    mark_attach_class: ClassDef,
    mark_glyph_sets: Vec<Coverage>,
}

impl GdefData {
    fn parse(bytes: &[u8]) -> Self {
        let Some(major) = u16::at(bytes, 0) else {
            return Self::default();
        };
        if major != 1 {
            return Self::default();
        }
        let minor = u16::at(bytes, 2).unwrap_or(0);
        let gc_off = usize::from(u16::at(bytes, 4).unwrap_or(0));
        let mac_off = usize::from(u16::at(bytes, 10).unwrap_or(0));
        let glyph_class = if gc_off > 0 {
            ClassDef::parse(bytes, gc_off).unwrap_or_default()
        } else {
            ClassDef::default()
        };
        let mark_attach_class = if mac_off > 0 {
            ClassDef::parse(bytes, mac_off).unwrap_or_default()
        } else {
            ClassDef::default()
        };
        let mut mark_glyph_sets = Vec::new();
        if minor >= 2 {
            let mgs_off = usize::from(u16::at(bytes, 12).unwrap_or(0));
            if mgs_off > 0 {
                if let Some(count) = u16::at(bytes, mgs_off + 2) {
                    for i in 0..usize::from(count) {
                        if let Some(rel) = u32::at(bytes, mgs_off + 4 + i * 4) {
                            if let Ok(rel_u) = usize::try_from(rel) {
                                let cov = mgs_off
                                    .checked_add(rel_u)
                                    .and_then(|o| Coverage::parse(bytes, o))
                                    .unwrap_or_default();
                                mark_glyph_sets.push(cov);
                            }
                        }
                    }
                }
            }
        }
        Self {
            glyph_class,
            mark_attach_class,
            mark_glyph_sets,
        }
    }

    #[inline]
    fn glyph_class(&self, gid: u16) -> u16 {
        self.glyph_class.get(gid)
    }

    #[inline]
    fn is_ignored(&self, gid: u16, flag: u16, mark_filter_set: Option<u16>) -> bool {
        if flag == 0 {
            return false;
        }
        let cls = self.glyph_class(gid);
        if (flag & FLAG_IGNORE_BASE) != 0 && cls == 1 {
            return true;
        }
        if (flag & FLAG_IGNORE_LIGATURES) != 0 && cls == 2 {
            return true;
        }
        if cls == 3 {
            if (flag & FLAG_IGNORE_MARKS) != 0 {
                return true;
            }
            let attach_cls = (flag & FLAG_MARK_ATTACHMENT_MASK) >> 8;
            if attach_cls != 0 && self.mark_attach_class.get(gid) != attach_cls {
                return true;
            }
            if (flag & FLAG_USE_MARK_FILTERING_SET) != 0 {
                if let Some(set_idx) = mark_filter_set {
                    if let Some(cov) = self.mark_glyph_sets.get(usize::from(set_idx)) {
                        if cov.get(gid).is_none() {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }
}

// ---------- Coverage & ClassDef ----------

#[derive(Debug, Clone, Default)]
enum Coverage {
    #[default]
    Empty,
    List(Vec<u16>),
    Ranges(Vec<(u16, u16, u16)>),
}

impl Coverage {
    fn parse(bytes: &[u8], off: usize) -> Option<Self> {
        let fmt = u16::at(bytes, off)?;
        let count = usize::from(u16::at(bytes, off + 2)?);
        match fmt {
            1 => {
                let mut list = Vec::with_capacity(count.min(4_096));
                for i in 0..count {
                    list.push(u16::at(bytes, off + 4 + i * 2)?);
                }
                Some(Self::List(list))
            }
            2 => {
                let mut ranges = Vec::with_capacity(count.min(4_096));
                for i in 0..count {
                    let base = off + 4 + i * 6;
                    let start = u16::at(bytes, base)?;
                    let end = u16::at(bytes, base + 2)?;
                    let start_cov = u16::at(bytes, base + 4)?;
                    if start <= end {
                        ranges.push((start, end, start_cov));
                    }
                }
                Some(Self::Ranges(ranges))
            }
            _ => None,
        }
    }

    #[inline]
    fn get(&self, gid: u16) -> Option<u16> {
        match self {
            Self::Empty => None,
            Self::List(list) => list.binary_search(&gid).ok().map(|i| i as u16),
            Self::Ranges(ranges) => {
                let idx = ranges.partition_point(|&(start, _, _)| start <= gid);
                if idx == 0 {
                    return None;
                }
                let (start, end, start_cov) = ranges[idx - 1];
                if gid <= end {
                    start_cov.checked_add(gid - start)
                } else {
                    None
                }
            }
        }
    }

    fn glyph_count_estimate(&self) -> usize {
        match self {
            Self::Empty => 0,
            Self::List(list) => list.len(),
            Self::Ranges(ranges) => ranges.iter().map(|&(s, e, _)| usize::from(e - s) + 1).sum(),
        }
    }

    fn to_mask(&self) -> CovMask {
        let mut mask = CovMask::empty();
        match self {
            Self::Empty => {}
            Self::List(list) => {
                for &g in list {
                    mask.insert(g);
                }
            }
            Self::Ranges(ranges) => {
                let mut count = 0usize;
                for &(s, e, _) in ranges {
                    for g in s..=e {
                        mask.insert(g);
                        count += 1;
                        if count >= 1_024 {
                            return CovMask::all();
                        }
                    }
                }
            }
        }
        mask
    }
}

#[derive(Debug, Clone, Default)]
enum ClassDef {
    #[default]
    Empty,
    Array {
        start_glyph: u16,
        classes: Vec<u16>,
    },
    Ranges(Vec<(u16, u16, u16)>),
}

impl ClassDef {
    fn parse(bytes: &[u8], off: usize) -> Option<Self> {
        let fmt = u16::at(bytes, off)?;
        match fmt {
            1 => {
                let start_glyph = u16::at(bytes, off + 2)?;
                let count = usize::from(u16::at(bytes, off + 4)?);
                let mut classes = Vec::with_capacity(count.min(8_192));
                for i in 0..count {
                    classes.push(u16::at(bytes, off + 6 + i * 2)?);
                }
                Some(Self::Array {
                    start_glyph,
                    classes,
                })
            }
            2 => {
                let count = usize::from(u16::at(bytes, off + 2)?);
                let mut ranges = Vec::with_capacity(count.min(8_192));
                for i in 0..count {
                    let base = off + 4 + i * 6;
                    let start = u16::at(bytes, base)?;
                    let end = u16::at(bytes, base + 2)?;
                    let cls = u16::at(bytes, base + 4)?;
                    if start <= end {
                        ranges.push((start, end, cls));
                    }
                }
                Some(Self::Ranges(ranges))
            }
            _ => None,
        }
    }

    #[inline]
    fn get(&self, gid: u16) -> u16 {
        match self {
            Self::Empty => 0,
            Self::Array {
                start_glyph,
                classes,
            } => gid
                .checked_sub(*start_glyph)
                .and_then(|d| classes.get(usize::from(d)).copied())
                .unwrap_or(0),
            Self::Ranges(ranges) => {
                let idx = ranges.partition_point(|&(start, _, _)| start <= gid);
                if idx == 0 {
                    return 0;
                }
                let (_, end, cls) = ranges[idx - 1];
                if gid <= end {
                    cls
                } else {
                    0
                }
            }
        }
    }
}

// ---------- Script / Feature / Lookup Index Parsing ----------

#[derive(Debug, Clone)]
struct LayoutTable {
    scripts: BTreeMap<[u8; 4], Vec<u16>>,
    features: Vec<([u8; 4], Vec<u16>)>,
    lookups: Vec<Lookup>,
}

#[derive(Debug, Clone)]
struct Lookup {
    flag: u16,
    mark_filtering_set: Option<u16>,
    trigger_mask: CovMask,
    sub_masks: Vec<(CovMask, CovMask)>,
    subtables: Vec<Subtable>,
}

impl LayoutTable {
    fn parse(bytes: &[u8], is_gsub: bool) -> Option<Self> {
        let major = u16::at(bytes, 0)?;
        if major != 1 {
            return None;
        }
        let script_list_off = usize::from(u16::at(bytes, 4)?);
        let feature_list_off = usize::from(u16::at(bytes, 6)?);
        let lookup_list_off = usize::from(u16::at(bytes, 8)?);

        // ScriptList -> DefaultLangSys feature indices
        let mut scripts = BTreeMap::new();
        if let Some(scount) = u16::at(bytes, script_list_off) {
            for i in 0..usize::from(scount) {
                let rec = script_list_off + 2 + i * 6;
                let tag_slice = bytes.get(rec..rec + 4)?;
                let mut tag = [0u8; 4];
                tag.copy_from_slice(tag_slice);
                let s_off = script_list_off + usize::from(u16::at(bytes, rec + 4)?);
                let dflt_lang = usize::from(u16::at(bytes, s_off).unwrap_or(0));
                let lang_off = if dflt_lang > 0 {
                    s_off + dflt_lang
                } else {
                    let lcnt = usize::from(u16::at(bytes, s_off + 2).unwrap_or(0));
                    if lcnt > 0 {
                        s_off + usize::from(u16::at(bytes, s_off + 8).unwrap_or(0))
                    } else {
                        0
                    }
                };
                if lang_off > 0 {
                    let mut feat_idxs = Vec::new();
                    let req = u16::at(bytes, lang_off + 2).unwrap_or(0xFFFF);
                    if req != 0xFFFF {
                        feat_idxs.push(req);
                    }
                    let fcnt = usize::from(u16::at(bytes, lang_off + 4).unwrap_or(0));
                    for j in 0..fcnt {
                        if let Some(fi) = u16::at(bytes, lang_off + 6 + j * 2) {
                            feat_idxs.push(fi);
                        }
                    }
                    scripts.insert(tag, feat_idxs);
                }
            }
        }

        // FeatureList
        let mut features = Vec::new();
        if let Some(fcount) = u16::at(bytes, feature_list_off) {
            for i in 0..usize::from(fcount) {
                let rec = feature_list_off + 2 + i * 6;
                let tag_slice = bytes.get(rec..rec + 4)?;
                let mut tag = [0u8; 4];
                tag.copy_from_slice(tag_slice);
                let f_off = feature_list_off + usize::from(u16::at(bytes, rec + 4)?);
                let lcnt = usize::from(u16::at(bytes, f_off + 2).unwrap_or(0));
                let mut lookup_idxs = Vec::with_capacity(lcnt);
                for j in 0..lcnt {
                    if let Some(li) = u16::at(bytes, f_off + 4 + j * 2) {
                        lookup_idxs.push(li);
                    }
                }
                features.push((tag, lookup_idxs));
            }
        }

        // LookupList
        let mut lookups = Vec::new();
        let lcount = usize::from(u16::at(bytes, lookup_list_off)?);
        lookups.reserve(lcount);
        for i in 0..lcount {
            let lo = lookup_list_off + usize::from(u16::at(bytes, lookup_list_off + 2 + i * 2)?);
            let ltype = u16::at(bytes, lo)?;
            let flag = u16::at(bytes, lo + 2)?;
            let scount = usize::from(u16::at(bytes, lo + 4)?);
            let mark_filtering_set = if (flag & FLAG_USE_MARK_FILTERING_SET) != 0 {
                u16::at(bytes, lo + 6 + scount * 2)
            } else {
                None
            };
            let mut subtables = Vec::with_capacity(scount);
            let mut sub_masks = Vec::with_capacity(scount);
            let mut trigger_mask = CovMask::empty();
            for si in 0..scount {
                let sub_off = lo + usize::from(u16::at(bytes, lo + 6 + si * 2)?);
                if let Some(st) = Subtable::parse(bytes, sub_off, ltype, is_gsub) {
                    let nec = st.necessary_mask();
                    let first = st.first_mask();
                    trigger_mask.union_with(&nec);
                    sub_masks.push((nec, first));
                    subtables.push(st);
                }
            }
            lookups.push(Lookup {
                flag,
                mark_filtering_set,
                trigger_mask,
                sub_masks,
                subtables,
            });
        }

        Some(Self {
            scripts,
            features,
            lookups,
        })
    }

    fn collect_lookups(&self, script: [u8; 4], feature_tags: &[[u8; 4]]) -> Vec<u16> {
        let feat_indices = self
            .scripts
            .get(&script)
            .or_else(|| self.scripts.get(b"latn"))
            .or_else(|| self.scripts.get(b"DFLT"));
        let mut out = Vec::new();
        if let Some(indices) = feat_indices {
            for &fi in indices {
                if let Some((tag, lookups)) = self.features.get(usize::from(fi)) {
                    if feature_tags.contains(tag) {
                        out.extend_from_slice(lookups);
                    }
                }
            }
        } else {
            for (tag, lookups) in &self.features {
                if feature_tags.contains(tag) {
                    out.extend_from_slice(lookups);
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    fn apply_gsub_lookup(
        &self,
        lookup_idx: u16,
        gdef: &GdefData,
        buf: &mut Vec<GlyphSlot>,
        buf_mask: &CovMask,
        req_mask: u32,
        depth: usize,
    ) -> bool {
        if depth >= MAX_RECURSE_DEPTH {
            return false;
        }
        let Some(lookup) = self.lookups.get(usize::from(lookup_idx)) else {
            return false;
        };
        let mut cur_mask = *buf_mask;
        let mut active: Vec<usize> = lookup
            .sub_masks
            .iter()
            .enumerate()
            .filter(|(_, (nec, _))| nec.intersects(&cur_mask))
            .map(|(idx, _)| idx)
            .collect();
        if active.is_empty() {
            return false;
        }
        let flag = lookup.flag;
        let mfs = lookup.mark_filtering_set;
        let mut any_matched = false;
        let mut i = 0usize;
        while i < buf.len() {
            let gid = buf[i].gid;
            if (buf[i].mask & req_mask) == 0 || (flag != 0 && gdef.is_ignored(gid, flag, mfs)) {
                i += 1;
                continue;
            }
            let mut matched = false;
            let mut next_mask = None;
            for &si in &active {
                if !lookup.sub_masks[si].1.contains(gid) {
                    continue;
                }
                let sub = &lookup.subtables[si];
                if let Some(advance_slots) =
                    sub.apply_gsub(self, gdef, buf, i, flag, mfs, req_mask, depth)
                {
                    i += advance_slots.max(1);
                    matched = true;
                    any_matched = true;
                    let candidate = buffer_mask(buf);
                    if candidate != cur_mask {
                        next_mask = Some(candidate);
                    }
                    break;
                }
            }
            if let Some(m) = next_mask {
                cur_mask = m;
                active = lookup
                    .sub_masks
                    .iter()
                    .enumerate()
                    .filter(|(_, (nec, _))| nec.intersects(&cur_mask))
                    .map(|(idx, _)| idx)
                    .collect();
            }
            if !matched {
                i += 1;
            }
        }
        any_matched
    }

    fn apply_gpos_lookup(
        &self,
        lookup_idx: u16,
        gdef: &GdefData,
        buf: &mut [GlyphSlot],
        buf_mask: &CovMask,
        depth: usize,
    ) -> bool {
        if depth >= MAX_RECURSE_DEPTH {
            return false;
        }
        let Some(lookup) = self.lookups.get(usize::from(lookup_idx)) else {
            return false;
        };
        let active: Vec<usize> = lookup
            .sub_masks
            .iter()
            .enumerate()
            .filter(|(_, (nec, _))| nec.intersects(buf_mask))
            .map(|(idx, _)| idx)
            .collect();
        if active.is_empty() {
            return false;
        }
        let flag = lookup.flag;
        let mfs = lookup.mark_filtering_set;
        let rtl = (flag & FLAG_RIGHT_TO_LEFT) != 0;
        let mut any_attached = false;
        let mut i = 0usize;
        while i < buf.len() {
            let gid = buf[i].gid;
            if flag != 0 && gdef.is_ignored(gid, flag, mfs) {
                i += 1;
                continue;
            }
            let mut matched = false;
            for &si in &active {
                if !lookup.sub_masks[si].1.contains(gid) {
                    continue;
                }
                let sub = &lookup.subtables[si];
                if let Some(step) = sub.apply_gpos(self, gdef, buf, i, flag, mfs, rtl, depth) {
                    if matches!(
                        sub,
                        Subtable::GposMarkBase(_)
                            | Subtable::GposMarkLig(_)
                            | Subtable::GposMarkMark(_)
                    ) {
                        any_attached = true;
                    }
                    i += step.max(1);
                    matched = true;
                    break;
                }
            }
            if !matched {
                i += 1;
            }
        }
        any_attached
    }
}

// ---------- Subtable Types ----------

#[derive(Debug, Clone)]
enum Subtable {
    GsubSingle(SingleSubst),
    GsubMultiple(MultipleSubst),
    GsubAlternate(AlternateSubst),
    GsubLigature(LigatureSubst),
    GsubContext(ContextTable),
    GsubChainContext(ChainContextTable),
    GposSingle(SinglePos),
    GposPair(PairPos),
    GposMarkBase(MarkAttachPos),
    GposMarkLig(MarkLigPos),
    GposMarkMark(MarkAttachPos),
}

impl Subtable {
    fn parse(bytes: &[u8], off: usize, ltype: u16, is_gsub: bool) -> Option<Self> {
        if is_gsub {
            if ltype == 7 {
                let ext_type = u16::at(bytes, off + 2)?;
                let rel = usize::try_from(u32::at(bytes, off + 4)?).ok()?;
                return Self::parse(bytes, off.checked_add(rel)?, ext_type, true);
            }
            match ltype {
                1 => SingleSubst::parse(bytes, off).map(Self::GsubSingle),
                2 => MultipleSubst::parse(bytes, off).map(Self::GsubMultiple),
                3 => AlternateSubst::parse(bytes, off).map(Self::GsubAlternate),
                4 => LigatureSubst::parse(bytes, off).map(Self::GsubLigature),
                5 => ContextTable::parse(bytes, off).map(Self::GsubContext),
                6 => ChainContextTable::parse(bytes, off).map(Self::GsubChainContext),
                _ => None,
            }
        } else {
            if ltype == 9 {
                let ext_type = u16::at(bytes, off + 2)?;
                let rel = usize::try_from(u32::at(bytes, off + 4)?).ok()?;
                return Self::parse(bytes, off.checked_add(rel)?, ext_type, false);
            }
            match ltype {
                1 => SinglePos::parse(bytes, off).map(Self::GposSingle),
                2 => PairPos::parse(bytes, off).map(Self::GposPair),
                4 => MarkAttachPos::parse(bytes, off).map(Self::GposMarkBase),
                5 => MarkLigPos::parse(bytes, off).map(Self::GposMarkLig),
                6 => MarkAttachPos::parse(bytes, off).map(Self::GposMarkMark),
                _ => None,
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_gsub(
        &self,
        table: &LayoutTable,
        gdef: &GdefData,
        buf: &mut Vec<GlyphSlot>,
        at: usize,
        flag: u16,
        mfs: Option<u16>,
        req_mask: u32,
        depth: usize,
    ) -> Option<usize> {
        match self {
            Self::GsubSingle(s) => s.apply(buf, at),
            Self::GsubMultiple(s) => s.apply(buf, at),
            Self::GsubAlternate(s) => s.apply(buf, at),
            Self::GsubLigature(s) => s.apply(gdef, buf, at, flag, mfs),
            Self::GsubContext(s) => s.apply_gsub(table, gdef, buf, at, flag, mfs, req_mask, depth),
            Self::GsubChainContext(s) => {
                s.apply_gsub(table, gdef, buf, at, flag, mfs, req_mask, depth)
            }
            _ => None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_gpos(
        &self,
        _table: &LayoutTable,
        gdef: &GdefData,
        buf: &mut [GlyphSlot],
        at: usize,
        flag: u16,
        mfs: Option<u16>,
        _rtl: bool,
        _depth: usize,
    ) -> Option<usize> {
        match self {
            Self::GposSingle(s) => s.apply(buf, at),
            Self::GposPair(s) => s.apply(gdef, buf, at, flag, mfs),
            Self::GposMarkBase(s) => s.apply_base(gdef, buf, at),
            Self::GposMarkLig(s) => s.apply(gdef, buf, at),
            Self::GposMarkMark(s) => s.apply_mark_to_mark(gdef, buf, at, flag, mfs),
            _ => None,
        }
    }

    fn necessary_mask(&self) -> CovMask {
        let smallest_cov_mask = |covs: &[Coverage]| -> CovMask {
            covs.iter()
                .min_by_key(|c| c.glyph_count_estimate())
                .map_or_else(CovMask::empty, Coverage::to_mask)
        };
        match self {
            Self::GsubSingle(s) => match s {
                SingleSubst::Delta { cov, .. } | SingleSubst::Array { cov, .. } => cov.to_mask(),
            },
            Self::GsubMultiple(s) => s.cov.to_mask(),
            Self::GsubAlternate(s) => s.cov.to_mask(),
            Self::GsubLigature(s) => {
                let first_cnt = s.cov.glyph_count_estimate();
                let mut second_mask = CovMask::empty();
                let mut has_single_comp = false;
                for rules in &s.sets {
                    for rule in rules {
                        if let Some(&g1) = rule.components.first() {
                            second_mask.insert(g1);
                        } else {
                            has_single_comp = true;
                        }
                    }
                }
                let second_cnt = second_mask.count_ones();
                if !has_single_comp && second_cnt > 0 && second_cnt < first_cnt {
                    second_mask
                } else {
                    s.cov.to_mask()
                }
            }
            Self::GsubContext(ContextTable::Fmt3 { input, .. }) => smallest_cov_mask(input),
            Self::GsubChainContext(cc) => match cc {
                ChainContextTable::Fmt1 { cov, .. } | ChainContextTable::Fmt2 { cov, .. } => {
                    cov.to_mask()
                }
                ChainContextTable::Fmt3 {
                    backtrack,
                    input,
                    lookahead,
                    ..
                } => {
                    let best_b = backtrack.iter().min_by_key(|c| c.glyph_count_estimate());
                    let best_i = input.iter().min_by_key(|c| c.glyph_count_estimate());
                    let best_l = lookahead.iter().min_by_key(|c| c.glyph_count_estimate());
                    [best_b, best_i, best_l]
                        .into_iter()
                        .flatten()
                        .min_by_key(|c| c.glyph_count_estimate())
                        .map_or_else(CovMask::empty, Coverage::to_mask)
                }
            },
            Self::GposSingle(s) => match s {
                SinglePos::Fmt1 { cov, .. } | SinglePos::Fmt2 { cov, .. } => cov.to_mask(),
            },
            Self::GposPair(p) => match p {
                PairPos::Fmt1 { cov, .. } | PairPos::Fmt2 { cov, .. } => cov.to_mask(),
            },
            Self::GposMarkBase(m) | Self::GposMarkMark(m) => m.mark_cov.to_mask(),
            Self::GposMarkLig(m) => m.mark_cov.to_mask(),
        }
    }

    fn first_mask(&self) -> CovMask {
        match self {
            Self::GsubSingle(s) => match s {
                SingleSubst::Delta { cov, .. } | SingleSubst::Array { cov, .. } => cov.to_mask(),
            },
            Self::GsubMultiple(s) => s.cov.to_mask(),
            Self::GsubAlternate(s) => s.cov.to_mask(),
            Self::GsubLigature(s) => s.cov.to_mask(),
            Self::GsubContext(ContextTable::Fmt3 { input, .. }) => match input.first() {
                Some(first) => first.to_mask(),
                None => CovMask::empty(),
            },
            Self::GsubChainContext(cc) => match cc {
                ChainContextTable::Fmt1 { cov, .. } | ChainContextTable::Fmt2 { cov, .. } => {
                    cov.to_mask()
                }
                ChainContextTable::Fmt3 { input, .. } => match input.first() {
                    Some(first) => first.to_mask(),
                    None => CovMask::empty(),
                },
            },
            Self::GposSingle(s) => match s {
                SinglePos::Fmt1 { cov, .. } | SinglePos::Fmt2 { cov, .. } => cov.to_mask(),
            },
            Self::GposPair(p) => match p {
                PairPos::Fmt1 { cov, .. } | PairPos::Fmt2 { cov, .. } => cov.to_mask(),
            },
            Self::GposMarkBase(m) | Self::GposMarkMark(m) => m.mark_cov.to_mask(),
            Self::GposMarkLig(m) => m.mark_cov.to_mask(),
        }
    }
}

// ---------- GSUB Subtables ----------

#[derive(Debug, Clone)]
enum SingleSubst {
    Delta { cov: Coverage, delta: i16 },
    Array { cov: Coverage, subst: Vec<u16> },
}

impl SingleSubst {
    fn parse(bytes: &[u8], off: usize) -> Option<Self> {
        let fmt = u16::at(bytes, off)?;
        let cov = Coverage::parse(bytes, off + usize::from(u16::at(bytes, off + 2)?))?;
        match fmt {
            1 => {
                let delta = i16::at(bytes, off + 4)?;
                Some(Self::Delta { cov, delta })
            }
            2 => {
                let cnt = usize::from(u16::at(bytes, off + 4)?);
                let mut subst = Vec::with_capacity(cnt.min(8_192));
                for i in 0..cnt {
                    subst.push(u16::at(bytes, off + 6 + i * 2)?);
                }
                Some(Self::Array { cov, subst })
            }
            _ => None,
        }
    }

    fn apply(&self, buf: &mut [GlyphSlot], at: usize) -> Option<usize> {
        let gid = buf[at].gid;
        match self {
            Self::Delta { cov, delta } => {
                cov.get(gid)?;
                buf[at].gid = gid.wrapping_add_signed(*delta);
                Some(1)
            }
            Self::Array { cov, subst } => {
                let idx = usize::from(cov.get(gid)?);
                buf[at].gid = *subst.get(idx)?;
                Some(1)
            }
        }
    }
}

#[derive(Debug, Clone)]
struct MultipleSubst {
    cov: Coverage,
    seqs: Vec<Vec<u16>>,
}

impl MultipleSubst {
    fn parse(bytes: &[u8], off: usize) -> Option<Self> {
        let fmt = u16::at(bytes, off)?;
        if fmt != 1 {
            return None;
        }
        let cov = Coverage::parse(bytes, off + usize::from(u16::at(bytes, off + 2)?))?;
        let cnt = usize::from(u16::at(bytes, off + 4)?);
        let mut seqs = Vec::with_capacity(cnt.min(4_096));
        for i in 0..cnt {
            let so = off + usize::from(u16::at(bytes, off + 6 + i * 2)?);
            let gcnt = usize::from(u16::at(bytes, so)?);
            let mut gids = Vec::with_capacity(gcnt.min(16));
            for j in 0..gcnt {
                gids.push(u16::at(bytes, so + 2 + j * 2)?);
            }
            seqs.push(gids);
        }
        Some(Self { cov, seqs })
    }

    fn apply(&self, buf: &mut Vec<GlyphSlot>, at: usize) -> Option<usize> {
        let idx = usize::from(self.cov.get(buf[at].gid)?);
        let seq = self.seqs.get(idx)?;
        if seq.is_empty() || buf.len() + seq.len() - 1 > MAX_OUTPUT_GLYPHS {
            return None;
        }
        let base = buf[at];
        buf[at].gid = seq[0];
        for (k, &gid) in seq.iter().enumerate().skip(1) {
            let mut copy = base;
            copy.gid = gid;
            copy.cluster_span = 0;
            buf.insert(at + k, copy);
        }
        Some(seq.len())
    }
}

#[derive(Debug, Clone)]
struct AlternateSubst {
    cov: Coverage,
    first_alts: Vec<u16>,
}

impl AlternateSubst {
    fn parse(bytes: &[u8], off: usize) -> Option<Self> {
        let fmt = u16::at(bytes, off)?;
        if fmt != 1 {
            return None;
        }
        let cov = Coverage::parse(bytes, off + usize::from(u16::at(bytes, off + 2)?))?;
        let cnt = usize::from(u16::at(bytes, off + 4)?);
        let mut first_alts = Vec::with_capacity(cnt.min(4_096));
        for i in 0..cnt {
            let ao = off + usize::from(u16::at(bytes, off + 6 + i * 2)?);
            let gcnt = u16::at(bytes, ao)?;
            let first = if gcnt > 0 {
                u16::at(bytes, ao + 2).unwrap_or(0)
            } else {
                0
            };
            first_alts.push(first);
        }
        Some(Self { cov, first_alts })
    }

    fn apply(&self, buf: &mut [GlyphSlot], at: usize) -> Option<usize> {
        let idx = usize::from(self.cov.get(buf[at].gid)?);
        let alt = *self.first_alts.get(idx)?;
        if alt == 0 {
            return None;
        }
        buf[at].gid = alt;
        Some(1)
    }
}

#[derive(Debug, Clone)]
struct LigatureRule {
    lig_glyph: u16,
    components: Vec<u16>,
}

#[derive(Debug, Clone)]
struct LigatureSubst {
    cov: Coverage,
    sets: Vec<Vec<LigatureRule>>,
}

impl LigatureSubst {
    fn parse(bytes: &[u8], off: usize) -> Option<Self> {
        let fmt = u16::at(bytes, off)?;
        if fmt != 1 {
            return None;
        }
        let cov = Coverage::parse(bytes, off + usize::from(u16::at(bytes, off + 2)?))?;
        let scnt = usize::from(u16::at(bytes, off + 4)?);
        let mut sets = Vec::with_capacity(scnt.min(4_096));
        for i in 0..scnt {
            let lset_off = off + usize::from(u16::at(bytes, off + 6 + i * 2)?);
            let lcnt = usize::from(u16::at(bytes, lset_off)?);
            let mut rules = Vec::with_capacity(lcnt.min(64));
            for j in 0..lcnt {
                let l_off = lset_off + usize::from(u16::at(bytes, lset_off + 2 + j * 2)?);
                let lig_glyph = u16::at(bytes, l_off)?;
                let comp_cnt = usize::from(u16::at(bytes, l_off + 2)?);
                if comp_cnt == 0 {
                    continue;
                }
                let mut components = Vec::with_capacity(comp_cnt - 1);
                for k in 0..(comp_cnt - 1) {
                    components.push(u16::at(bytes, l_off + 4 + k * 2)?);
                }
                rules.push(LigatureRule {
                    lig_glyph,
                    components,
                });
            }
            sets.push(rules);
        }
        Some(Self { cov, sets })
    }

    fn apply(
        &self,
        gdef: &GdefData,
        buf: &mut Vec<GlyphSlot>,
        at: usize,
        flag: u16,
        mfs: Option<u16>,
    ) -> Option<usize> {
        let idx = usize::from(self.cov.get(buf[at].gid)?);
        let rules = self.sets.get(idx)?;
        for rule in rules {
            let mut matched_indices = Vec::with_capacity(rule.components.len());
            let mut cur = at + 1;
            let mut ok = true;
            for &want in &rule.components {
                while cur < buf.len() && gdef.is_ignored(buf[cur].gid, flag, mfs) {
                    cur += 1;
                }
                if cur >= buf.len() || buf[cur].gid != want {
                    ok = false;
                    break;
                }
                matched_indices.push(cur);
                cur += 1;
            }
            if ok {
                let last = *matched_indices.last().unwrap_or(&at);
                let first_c = buf[at].cluster;
                let last_c_end = buf[last].cluster + buf[last].cluster_span;
                buf[at].gid = rule.lig_glyph;
                buf[at].cluster_span = last_c_end.saturating_sub(first_c).max(1);
                let max_lig = buf.iter().map(|s| s.lig_id).max().unwrap_or(0);
                let lig_id = max_lig.saturating_add(1);
                buf[at].lig_id = lig_id;
                // Reassign intervening marks to components before removing component base glyphs,
                // plus trailing marks belonging to the final ligature component.
                let mut prev_pos = at;
                for (comp_num, &m_idx) in (1u16..).zip(matched_indices.iter()) {
                    for slot in buf.iter_mut().take(m_idx).skip(prev_pos + 1) {
                        slot.lig_comp = comp_num - 1;
                        slot.lig_id = lig_id;
                    }
                    prev_pos = m_idx;
                }
                let final_comp = matched_indices.len() as u16;
                for slot in buf.iter_mut().skip(last + 1) {
                    if gdef.glyph_class(slot.gid) != 3 {
                        break;
                    }
                    slot.lig_comp = final_comp;
                    slot.lig_id = lig_id;
                }
                for &m_idx in matched_indices.iter().rev() {
                    buf.remove(m_idx);
                }
                return Some(1);
            }
        }
        None
    }
}

// ---------- Contextual & Chaining Contextual GSUB (Types 5 & 6) ----------

#[derive(Debug, Clone)]
struct SeqLookupRecord {
    seq_index: usize,
    lookup_index: u16,
}

#[derive(Debug, Clone)]
enum ContextTable {
    Fmt3 {
        input: Vec<Coverage>,
        records: Vec<SeqLookupRecord>,
    },
}

impl ContextTable {
    fn parse(bytes: &[u8], off: usize) -> Option<Self> {
        let fmt = u16::at(bytes, off)?;
        if fmt != 3 {
            return None;
        }
        let gcnt = usize::from(u16::at(bytes, off + 2)?);
        let scnt = usize::from(u16::at(bytes, off + 4)?);
        if gcnt == 0 {
            return None;
        }
        let mut input = Vec::with_capacity(gcnt.min(32));
        for i in 0..gcnt {
            let cov = Coverage::parse(bytes, off + usize::from(u16::at(bytes, off + 6 + i * 2)?))?;
            input.push(cov);
        }
        let rec_base = off + 6 + gcnt * 2;
        let mut records = Vec::with_capacity(scnt.min(32));
        for i in 0..scnt {
            let seq_index = usize::from(u16::at(bytes, rec_base + i * 4)?);
            let lookup_index = u16::at(bytes, rec_base + i * 4 + 2)?;
            records.push(SeqLookupRecord {
                seq_index,
                lookup_index,
            });
        }
        Some(Self::Fmt3 { input, records })
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_gsub(
        &self,
        table: &LayoutTable,
        gdef: &GdefData,
        buf: &mut Vec<GlyphSlot>,
        at: usize,
        flag: u16,
        mfs: Option<u16>,
        req_mask: u32,
        depth: usize,
    ) -> Option<usize> {
        let Self::Fmt3 { input, records } = self;
        let mut positions = match_forward_cov(gdef, buf, at, input, flag, mfs, false)?;
        apply_nested_gsub(table, gdef, buf, &mut positions, records, req_mask, depth);
        Some(positions.len().max(1))
    }
}

#[derive(Debug, Clone)]
enum ChainMatcher {
    Glyph(u16),
    Class(u16),
}

#[derive(Debug, Clone)]
struct ChainRule {
    backtrack: Vec<ChainMatcher>,
    input: Vec<ChainMatcher>,
    lookahead: Vec<ChainMatcher>,
    records: Vec<SeqLookupRecord>,
}

#[derive(Debug, Clone)]
enum ChainContextTable {
    Fmt1 {
        cov: Coverage,
        sets: Vec<Vec<ChainRule>>,
    },
    Fmt2 {
        cov: Coverage,
        back_cls: ClassDef,
        in_cls: ClassDef,
        look_cls: ClassDef,
        sets: Vec<Vec<ChainRule>>,
    },
    Fmt3 {
        backtrack: Vec<Coverage>,
        input: Vec<Coverage>,
        lookahead: Vec<Coverage>,
        records: Vec<SeqLookupRecord>,
    },
}

impl ChainContextTable {
    fn parse(bytes: &[u8], off: usize) -> Option<Self> {
        let fmt = u16::at(bytes, off)?;
        match fmt {
            1 => {
                let cov = Coverage::parse(bytes, off + usize::from(u16::at(bytes, off + 2)?))?;
                let scnt = usize::from(u16::at(bytes, off + 4)?);
                let mut sets = Vec::with_capacity(scnt.min(2_048));
                for i in 0..scnt {
                    let set_off = off + usize::from(u16::at(bytes, off + 6 + i * 2)?);
                    let rcnt = usize::from(u16::at(bytes, set_off)?);
                    let mut rules = Vec::with_capacity(rcnt.min(64));
                    for j in 0..rcnt {
                        let r_off = set_off + usize::from(u16::at(bytes, set_off + 2 + j * 2)?);
                        if let Some(rule) = parse_chain_rule(bytes, r_off, false) {
                            rules.push(rule);
                        }
                    }
                    sets.push(rules);
                }
                Some(Self::Fmt1 { cov, sets })
            }
            2 => {
                let cov = Coverage::parse(bytes, off + usize::from(u16::at(bytes, off + 2)?))?;
                let back_cls = ClassDef::parse(bytes, off + usize::from(u16::at(bytes, off + 4)?))?;
                let in_cls = ClassDef::parse(bytes, off + usize::from(u16::at(bytes, off + 6)?))?;
                let look_cls = ClassDef::parse(bytes, off + usize::from(u16::at(bytes, off + 8)?))?;
                let scnt = usize::from(u16::at(bytes, off + 10)?);
                let mut sets = Vec::with_capacity(scnt.min(1_024));
                for i in 0..scnt {
                    let rel = usize::from(u16::at(bytes, off + 12 + i * 2)?);
                    if rel == 0 {
                        sets.push(Vec::new());
                        continue;
                    }
                    let set_off = off + rel;
                    let rcnt = usize::from(u16::at(bytes, set_off)?);
                    let mut rules = Vec::with_capacity(rcnt.min(64));
                    for j in 0..rcnt {
                        let r_off = set_off + usize::from(u16::at(bytes, set_off + 2 + j * 2)?);
                        if let Some(rule) = parse_chain_rule(bytes, r_off, true) {
                            rules.push(rule);
                        }
                    }
                    sets.push(rules);
                }
                Some(Self::Fmt2 {
                    cov,
                    back_cls,
                    in_cls,
                    look_cls,
                    sets,
                })
            }
            3 => {
                let bcnt = usize::from(u16::at(bytes, off + 2)?);
                let mut p = off + 4;
                let mut backtrack = Vec::with_capacity(bcnt.min(16));
                for _ in 0..bcnt {
                    backtrack.push(Coverage::parse(
                        bytes,
                        off + usize::from(u16::at(bytes, p)?),
                    )?);
                    p += 2;
                }
                let icnt = usize::from(u16::at(bytes, p)?);
                p += 2;
                if icnt == 0 {
                    return None;
                }
                let mut input = Vec::with_capacity(icnt.min(16));
                for _ in 0..icnt {
                    input.push(Coverage::parse(
                        bytes,
                        off + usize::from(u16::at(bytes, p)?),
                    )?);
                    p += 2;
                }
                let lcnt = usize::from(u16::at(bytes, p)?);
                p += 2;
                let mut lookahead = Vec::with_capacity(lcnt.min(16));
                for _ in 0..lcnt {
                    lookahead.push(Coverage::parse(
                        bytes,
                        off + usize::from(u16::at(bytes, p)?),
                    )?);
                    p += 2;
                }
                let rcnt = usize::from(u16::at(bytes, p)?);
                p += 2;
                let mut records = Vec::with_capacity(rcnt.min(32));
                for _ in 0..rcnt {
                    records.push(SeqLookupRecord {
                        seq_index: usize::from(u16::at(bytes, p)?),
                        lookup_index: u16::at(bytes, p + 2)?,
                    });
                    p += 4;
                }
                Some(Self::Fmt3 {
                    backtrack,
                    input,
                    lookahead,
                    records,
                })
            }
            _ => None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_gsub(
        &self,
        table: &LayoutTable,
        gdef: &GdefData,
        buf: &mut Vec<GlyphSlot>,
        at: usize,
        flag: u16,
        mfs: Option<u16>,
        req_mask: u32,
        depth: usize,
    ) -> Option<usize> {
        match self {
            Self::Fmt1 { cov, sets } => {
                let idx = usize::from(cov.get(buf[at].gid)?);
                for rule in sets.get(idx)? {
                    if let Some(mut positions) =
                        match_chain_rule(gdef, buf, at, rule, flag, mfs, None)
                    {
                        apply_nested_gsub(
                            table,
                            gdef,
                            buf,
                            &mut positions,
                            &rule.records,
                            req_mask,
                            depth,
                        );
                        return Some(positions.len().max(1));
                    }
                }
                None
            }
            Self::Fmt2 {
                cov,
                back_cls,
                in_cls,
                look_cls,
                sets,
            } => {
                cov.get(buf[at].gid)?;
                let c = usize::from(in_cls.get(buf[at].gid));
                for rule in sets.get(c)? {
                    if let Some(mut positions) = match_chain_rule(
                        gdef,
                        buf,
                        at,
                        rule,
                        flag,
                        mfs,
                        Some((back_cls, in_cls, look_cls)),
                    ) {
                        apply_nested_gsub(
                            table,
                            gdef,
                            buf,
                            &mut positions,
                            &rule.records,
                            req_mask,
                            depth,
                        );
                        return Some(positions.len().max(1));
                    }
                }
                None
            }
            Self::Fmt3 {
                backtrack,
                input,
                lookahead,
                records,
            } => {
                let mut positions = match_forward_cov(gdef, buf, at, input, flag, mfs, false)?;
                if !match_backward_cov(gdef, buf, at, backtrack, flag, mfs) {
                    return None;
                }
                let after = positions.last().copied().unwrap_or(at) + 1;
                if match_forward_cov(gdef, buf, after, lookahead, flag, mfs, true).is_none()
                    && !lookahead.is_empty()
                {
                    return None;
                }
                apply_nested_gsub(table, gdef, buf, &mut positions, records, req_mask, depth);
                Some(positions.len().max(1))
            }
        }
    }
}

fn parse_chain_rule(bytes: &[u8], off: usize, is_class: bool) -> Option<ChainRule> {
    let mk = |v: u16| {
        if is_class {
            ChainMatcher::Class(v)
        } else {
            ChainMatcher::Glyph(v)
        }
    };
    let mut p = off;
    let bcnt = usize::from(u16::at(bytes, p)?);
    p += 2;
    let mut backtrack = Vec::with_capacity(bcnt.min(16));
    for _ in 0..bcnt {
        backtrack.push(mk(u16::at(bytes, p)?));
        p += 2;
    }
    let icnt = usize::from(u16::at(bytes, p)?);
    p += 2;
    if icnt == 0 {
        return None;
    }
    let mut input = Vec::with_capacity(icnt - 1);
    for _ in 0..(icnt - 1) {
        input.push(mk(u16::at(bytes, p)?));
        p += 2;
    }
    let lcnt = usize::from(u16::at(bytes, p)?);
    p += 2;
    let mut lookahead = Vec::with_capacity(lcnt.min(16));
    for _ in 0..lcnt {
        lookahead.push(mk(u16::at(bytes, p)?));
        p += 2;
    }
    let scnt = usize::from(u16::at(bytes, p)?);
    p += 2;
    let mut records = Vec::with_capacity(scnt.min(32));
    for _ in 0..scnt {
        records.push(SeqLookupRecord {
            seq_index: usize::from(u16::at(bytes, p)?),
            lookup_index: u16::at(bytes, p + 2)?,
        });
        p += 4;
    }
    Some(ChainRule {
        backtrack,
        input,
        lookahead,
        records,
    })
}

fn match_forward_cov(
    gdef: &GdefData,
    buf: &[GlyphSlot],
    start: usize,
    covs: &[Coverage],
    flag: u16,
    mfs: Option<u16>,
    skip_first: bool,
) -> Option<Vec<usize>> {
    if covs.is_empty() {
        return Some(Vec::new());
    }
    let mut positions = Vec::with_capacity(covs.len());
    let mut cur = start;
    for (k, cov) in covs.iter().enumerate() {
        if k > 0 || skip_first {
            while cur < buf.len() && gdef.is_ignored(buf[cur].gid, flag, mfs) {
                cur += 1;
            }
        }
        if cur >= buf.len() {
            return None;
        }
        cov.get(buf[cur].gid)?;
        positions.push(cur);
        cur += 1;
    }
    Some(positions)
}

fn match_backward_cov(
    gdef: &GdefData,
    buf: &[GlyphSlot],
    at: usize,
    back: &[Coverage],
    flag: u16,
    mfs: Option<u16>,
) -> bool {
    let mut cur = at;
    for cov in back {
        loop {
            if cur == 0 {
                return false;
            }
            cur -= 1;
            if !gdef.is_ignored(buf[cur].gid, flag, mfs) {
                break;
            }
        }
        if cov.get(buf[cur].gid).is_none() {
            return false;
        }
    }
    true
}

fn match_chain_rule(
    gdef: &GdefData,
    buf: &[GlyphSlot],
    at: usize,
    rule: &ChainRule,
    flag: u16,
    mfs: Option<u16>,
    classes: Option<(&ClassDef, &ClassDef, &ClassDef)>,
) -> Option<Vec<usize>> {
    let matches_item = |gid: u16, matcher: &ChainMatcher, cls_def: Option<&ClassDef>| match matcher
    {
        ChainMatcher::Glyph(g) => gid == *g,
        ChainMatcher::Class(c) => cls_def.is_some_and(|cd| cd.get(gid) == *c),
    };
    let (bc_cls, in_cls, lk_cls) = match classes {
        Some((b, i, l)) => (Some(b), Some(i), Some(l)),
        None => (None, None, None),
    };
    let mut positions = vec![at];
    let mut cur = at + 1;
    for m in &rule.input {
        while cur < buf.len() && gdef.is_ignored(buf[cur].gid, flag, mfs) {
            cur += 1;
        }
        if cur >= buf.len() || !matches_item(buf[cur].gid, m, in_cls) {
            return None;
        }
        positions.push(cur);
        cur += 1;
    }
    // Backtrack (reverse order in OpenType spec from `at - 1` backward)
    let mut bcur = at;
    for m in &rule.backtrack {
        loop {
            if bcur == 0 {
                return None;
            }
            bcur -= 1;
            if !gdef.is_ignored(buf[bcur].gid, flag, mfs) {
                break;
            }
        }
        if !matches_item(buf[bcur].gid, m, bc_cls) {
            return None;
        }
    }
    // Lookahead
    let mut lcur = *positions.last().unwrap_or(&at) + 1;
    for m in &rule.lookahead {
        while lcur < buf.len() && gdef.is_ignored(buf[lcur].gid, flag, mfs) {
            lcur += 1;
        }
        if lcur >= buf.len() || !matches_item(buf[lcur].gid, m, lk_cls) {
            return None;
        }
        lcur += 1;
    }
    Some(positions)
}

fn apply_nested_gsub(
    table: &LayoutTable,
    gdef: &GdefData,
    buf: &mut Vec<GlyphSlot>,
    positions: &mut [usize],
    records: &[SeqLookupRecord],
    req_mask: u32,
    depth: usize,
) {
    if depth + 1 >= MAX_RECURSE_DEPTH {
        return;
    }
    for rec in records {
        let Some(&pos) = positions.get(rec.seq_index) else {
            continue;
        };
        if pos >= buf.len() {
            continue;
        }
        let before_len = buf.len();
        if let Some(lookup) = table.lookups.get(usize::from(rec.lookup_index)) {
            for sub in &lookup.subtables {
                if sub
                    .apply_gsub(
                        table,
                        gdef,
                        buf,
                        pos,
                        lookup.flag,
                        lookup.mark_filtering_set,
                        req_mask,
                        depth + 1,
                    )
                    .is_some()
                {
                    break;
                }
            }
        }
        let after_len = buf.len();
        if after_len != before_len {
            let delta = after_len as isize - before_len as isize;
            for p in positions.iter_mut() {
                if *p > pos {
                    *p = (*p as isize + delta).max(0) as usize;
                }
            }
        }
    }
}

// ---------- GPOS ValueRecords, Anchors, and Subtables ----------

#[derive(Debug, Clone, Copy, Default)]
struct ValueRecord {
    x_place: i16,
    y_place: i16,
    x_adv: i16,
    y_adv: i16,
}

impl ValueRecord {
    fn parse(bytes: &[u8], off: &mut usize, fmt: u16) -> Option<Self> {
        let mut out = Self::default();
        if (fmt & 0x0001) != 0 {
            out.x_place = i16::at(bytes, *off)?;
            *off += 2;
        }
        if (fmt & 0x0002) != 0 {
            out.y_place = i16::at(bytes, *off)?;
            *off += 2;
        }
        if (fmt & 0x0004) != 0 {
            out.x_adv = i16::at(bytes, *off)?;
            *off += 2;
        }
        if (fmt & 0x0008) != 0 {
            out.y_adv = i16::at(bytes, *off)?;
            *off += 2;
        }
        for bit in [0x0010u16, 0x0020, 0x0040, 0x0080] {
            if (fmt & bit) != 0 {
                *off += 2;
            }
        }
        Some(out)
    }

    #[inline]
    fn byte_len(fmt: u16) -> usize {
        ((fmt & 0x00FF).count_ones() as usize) * 2
    }

    fn apply_to(&self, slot: &mut GlyphSlot) {
        slot.x_offset += i32::from(self.x_place);
        slot.y_offset += i32::from(self.y_place);
        slot.x_advance += i32::from(self.x_adv);
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Anchor {
    x: i16,
    y: i16,
}

impl Anchor {
    fn parse(bytes: &[u8], off: usize) -> Option<Self> {
        let _fmt = u16::at(bytes, off)?;
        let x = i16::at(bytes, off + 2)?;
        let y = i16::at(bytes, off + 4)?;
        Some(Self { x, y })
    }
}

#[derive(Debug, Clone)]
enum SinglePos {
    Fmt1 {
        cov: Coverage,
        val: ValueRecord,
    },
    Fmt2 {
        cov: Coverage,
        vals: Vec<ValueRecord>,
    },
}

impl SinglePos {
    fn parse(bytes: &[u8], off: usize) -> Option<Self> {
        let fmt = u16::at(bytes, off)?;
        let cov = Coverage::parse(bytes, off + usize::from(u16::at(bytes, off + 2)?))?;
        let vf = u16::at(bytes, off + 4)?;
        let mut p = off + 6;
        match fmt {
            1 => Some(Self::Fmt1 {
                cov,
                val: ValueRecord::parse(bytes, &mut p, vf)?,
            }),
            2 => {
                let cnt = usize::from(u16::at(bytes, p)?);
                p += 2;
                let mut vals = Vec::with_capacity(cnt.min(8_192));
                for _ in 0..cnt {
                    vals.push(ValueRecord::parse(bytes, &mut p, vf)?);
                }
                Some(Self::Fmt2 { cov, vals })
            }
            _ => None,
        }
    }

    fn apply(&self, buf: &mut [GlyphSlot], at: usize) -> Option<usize> {
        match self {
            Self::Fmt1 { cov, val } => {
                cov.get(buf[at].gid)?;
                val.apply_to(&mut buf[at]);
                Some(1)
            }
            Self::Fmt2 { cov, vals } => {
                let idx = usize::from(cov.get(buf[at].gid)?);
                vals.get(idx)?.apply_to(&mut buf[at]);
                Some(1)
            }
        }
    }
}

#[derive(Debug, Clone)]
enum PairPos {
    Fmt1 {
        cov: Coverage,
        sets: Vec<Vec<(u16, ValueRecord, ValueRecord)>>,
    },
    Fmt2 {
        cov: Coverage,
        class1: ClassDef,
        class2: ClassDef,
        c1_count: usize,
        c2_count: usize,
        pairs: Vec<(ValueRecord, ValueRecord)>,
    },
}

impl PairPos {
    fn parse(bytes: &[u8], off: usize) -> Option<Self> {
        let fmt = u16::at(bytes, off)?;
        let cov = Coverage::parse(bytes, off + usize::from(u16::at(bytes, off + 2)?))?;
        let vf1 = u16::at(bytes, off + 4)?;
        let vf2 = u16::at(bytes, off + 6)?;
        match fmt {
            1 => {
                let psc = usize::from(u16::at(bytes, off + 8)?);
                let mut sets = Vec::with_capacity(psc.min(8_192));
                for i in 0..psc {
                    let ps_off = off + usize::from(u16::at(bytes, off + 10 + i * 2)?);
                    let pvc = usize::from(u16::at(bytes, ps_off)?);
                    let mut p = ps_off + 2;
                    let mut row = Vec::with_capacity(pvc.min(1_024));
                    for _ in 0..pvc {
                        let g2 = u16::at(bytes, p)?;
                        p += 2;
                        let v1 = ValueRecord::parse(bytes, &mut p, vf1)?;
                        let v2 = ValueRecord::parse(bytes, &mut p, vf2)?;
                        row.push((g2, v1, v2));
                    }
                    sets.push(row);
                }
                Some(Self::Fmt1 { cov, sets })
            }
            2 => {
                let class1 = ClassDef::parse(bytes, off + usize::from(u16::at(bytes, off + 8)?))?;
                let class2 = ClassDef::parse(bytes, off + usize::from(u16::at(bytes, off + 10)?))?;
                let c1_count = usize::from(u16::at(bytes, off + 12)?);
                let c2_count = usize::from(u16::at(bytes, off + 14)?);
                let total = c1_count.checked_mul(c2_count)?;
                if total > 250_000 {
                    return None;
                }
                let rec_size = ValueRecord::byte_len(vf1) + ValueRecord::byte_len(vf2);
                if off + 16 + total * rec_size > bytes.len() {
                    return None;
                }
                let mut p = off + 16;
                let mut pairs = Vec::with_capacity(total);
                for _ in 0..total {
                    let v1 = ValueRecord::parse(bytes, &mut p, vf1)?;
                    let v2 = ValueRecord::parse(bytes, &mut p, vf2)?;
                    pairs.push((v1, v2));
                }
                Some(Self::Fmt2 {
                    cov,
                    class1,
                    class2,
                    c1_count,
                    c2_count,
                    pairs,
                })
            }
            _ => None,
        }
    }

    #[inline]
    fn lookup_pair(&self, first: u16, second: u16) -> Option<(ValueRecord, ValueRecord)> {
        match self {
            Self::Fmt1 { cov, sets } => {
                let idx = usize::from(cov.get(first)?);
                let row = sets.get(idx)?;
                let pos = row.binary_search_by_key(&second, |&(g, _, _)| g).ok()?;
                let (_, v1, v2) = row[pos];
                Some((v1, v2))
            }
            Self::Fmt2 {
                cov,
                class1,
                class2,
                c1_count,
                c2_count,
                pairs,
            } => {
                cov.get(first)?;
                let c1 = usize::from(class1.get(first));
                let c2 = usize::from(class2.get(second));
                if c1 >= *c1_count || c2 >= *c2_count {
                    return None;
                }
                pairs.get(c1 * c2_count + c2).copied()
            }
        }
    }

    #[inline]
    fn eval_x_advances(&self, first: u16, second: u16) -> (i32, i32) {
        self.lookup_pair(first, second).map_or((0, 0), |(v1, v2)| {
            (i32::from(v1.x_adv), i32::from(v2.x_adv))
        })
    }

    fn apply(
        &self,
        gdef: &GdefData,
        buf: &mut [GlyphSlot],
        at: usize,
        flag: u16,
        mfs: Option<u16>,
    ) -> Option<usize> {
        let next = if flag == 0 {
            if at + 1 < buf.len() {
                at + 1
            } else {
                return None;
            }
        } else {
            let mut n = at + 1;
            while n < buf.len() && gdef.is_ignored(buf[n].gid, flag, mfs) {
                n += 1;
            }
            if n >= buf.len() {
                return None;
            }
            n
        };
        let (v1, v2) = self.lookup_pair(buf[at].gid, buf[next].gid)?;
        v1.apply_to(&mut buf[at]);
        v2.apply_to(&mut buf[next]);
        Some(1)
    }
}

#[derive(Debug, Clone)]
struct MarkAttachPos {
    mark_cov: Coverage,
    base_cov: Coverage,
    mark_records: Vec<(u16, Anchor)>,
    base_records: Vec<Vec<Option<Anchor>>>,
}

impl MarkAttachPos {
    fn parse(bytes: &[u8], off: usize) -> Option<Self> {
        let fmt = u16::at(bytes, off)?;
        if fmt != 1 {
            return None;
        }
        let mark_cov = Coverage::parse(bytes, off + usize::from(u16::at(bytes, off + 2)?))?;
        let base_cov = Coverage::parse(bytes, off + usize::from(u16::at(bytes, off + 4)?))?;
        let class_count = usize::from(u16::at(bytes, off + 6)?);
        let mark_arr_off = off + usize::from(u16::at(bytes, off + 8)?);
        let base_arr_off = off + usize::from(u16::at(bytes, off + 10)?);

        let mcnt = usize::from(u16::at(bytes, mark_arr_off)?);
        let mut mark_records = Vec::with_capacity(mcnt.min(4_096));
        for i in 0..mcnt {
            let rec = mark_arr_off + 2 + i * 4;
            let cls = u16::at(bytes, rec)?;
            let anc_rel = u16::at(bytes, rec + 2)?;
            let anchor = if anc_rel == 0 {
                Anchor::default()
            } else {
                Anchor::parse(bytes, mark_arr_off + usize::from(anc_rel))?
            };
            mark_records.push((cls, anchor));
        }

        let bcnt = usize::from(u16::at(bytes, base_arr_off)?);
        let mut base_records = Vec::with_capacity(bcnt.min(8_192));
        for i in 0..bcnt {
            let mut row = Vec::with_capacity(class_count);
            for c in 0..class_count {
                let slot = base_arr_off + 2 + (i * class_count + c) * 2;
                let rel = u16::at(bytes, slot)?;
                if rel == 0 {
                    row.push(None);
                } else {
                    row.push(Anchor::parse(bytes, base_arr_off + usize::from(rel)));
                }
            }
            base_records.push(row);
        }

        Some(Self {
            mark_cov,
            base_cov,
            mark_records,
            base_records,
        })
    }

    fn apply_base(&self, gdef: &GdefData, buf: &mut [GlyphSlot], at: usize) -> Option<usize> {
        let m_idx = usize::from(self.mark_cov.get(buf[at].gid)?);
        let (m_cls, m_anc) = *self.mark_records.get(m_idx)?;
        // Search backward for preceding non-mark base glyph.
        let mut prev = at;
        let base_idx = loop {
            if prev == 0 {
                return None;
            }
            prev -= 1;
            let gc = gdef.glyph_class(buf[prev].gid);
            if gc != 3 {
                break prev;
            }
        };
        let b_idx = usize::from(self.base_cov.get(buf[base_idx].gid)?);
        let b_anc = self
            .base_records
            .get(b_idx)?
            .get(usize::from(m_cls))?
            .as_ref()?;
        buf[at].x_offset = i32::from(b_anc.x) - i32::from(m_anc.x);
        buf[at].y_offset = i32::from(b_anc.y) - i32::from(m_anc.y);
        buf[at].x_advance = 0;
        buf[at].attach_to = Some(base_idx);
        Some(1)
    }

    fn apply_mark_to_mark(
        &self,
        gdef: &GdefData,
        buf: &mut [GlyphSlot],
        at: usize,
        flag: u16,
        mfs: Option<u16>,
    ) -> Option<usize> {
        if at == 0 {
            return None;
        }
        let m_idx = usize::from(self.mark_cov.get(buf[at].gid)?);
        let (m_cls, m_anc) = *self.mark_records.get(m_idx)?;
        let mut prev = at;
        let prev_mark = loop {
            if prev == 0 {
                return None;
            }
            prev -= 1;
            if !gdef.is_ignored(buf[prev].gid, flag, mfs) {
                break prev;
            }
        };
        if gdef.glyph_class(buf[prev_mark].gid) != 3
            || buf[prev_mark].lig_id != buf[at].lig_id
            || (buf[at].lig_id != 0 && buf[prev_mark].lig_comp != buf[at].lig_comp)
        {
            return None;
        }
        let b_idx = usize::from(self.base_cov.get(buf[prev_mark].gid)?);
        let b_anc = self
            .base_records
            .get(b_idx)?
            .get(usize::from(m_cls))?
            .as_ref()?;
        buf[at].x_offset = i32::from(b_anc.x) - i32::from(m_anc.x);
        buf[at].y_offset = i32::from(b_anc.y) - i32::from(m_anc.y);
        buf[at].x_advance = 0;
        buf[at].attach_to = Some(prev_mark);
        Some(1)
    }
}

#[derive(Debug, Clone)]
struct MarkLigPos {
    mark_cov: Coverage,
    lig_cov: Coverage,
    mark_records: Vec<(u16, Anchor)>,
    lig_records: Vec<Vec<Vec<Option<Anchor>>>>,
}

impl MarkLigPos {
    fn parse(bytes: &[u8], off: usize) -> Option<Self> {
        let fmt = u16::at(bytes, off)?;
        if fmt != 1 {
            return None;
        }
        let mark_cov = Coverage::parse(bytes, off + usize::from(u16::at(bytes, off + 2)?))?;
        let lig_cov = Coverage::parse(bytes, off + usize::from(u16::at(bytes, off + 4)?))?;
        let class_count = usize::from(u16::at(bytes, off + 6)?);
        let mark_arr_off = off + usize::from(u16::at(bytes, off + 8)?);
        let lig_arr_off = off + usize::from(u16::at(bytes, off + 10)?);

        let mcnt = usize::from(u16::at(bytes, mark_arr_off)?);
        let mut mark_records = Vec::with_capacity(mcnt.min(4_096));
        for i in 0..mcnt {
            let rec = mark_arr_off + 2 + i * 4;
            let cls = u16::at(bytes, rec)?;
            let rel = u16::at(bytes, rec + 2)?;
            let anchor = if rel == 0 {
                Anchor::default()
            } else {
                Anchor::parse(bytes, mark_arr_off + usize::from(rel))?
            };
            mark_records.push((cls, anchor));
        }

        let lcnt = usize::from(u16::at(bytes, lig_arr_off)?);
        let mut lig_records = Vec::with_capacity(lcnt.min(2_048));
        for i in 0..lcnt {
            let la_off = lig_arr_off + usize::from(u16::at(bytes, lig_arr_off + 2 + i * 2)?);
            let comp_cnt = usize::from(u16::at(bytes, la_off)?);
            let mut comps = Vec::with_capacity(comp_cnt);
            for comp in 0..comp_cnt {
                let mut row = Vec::with_capacity(class_count);
                for c in 0..class_count {
                    let slot = la_off + 2 + (comp * class_count + c) * 2;
                    let rel = u16::at(bytes, slot)?;
                    row.push(if rel == 0 {
                        None
                    } else {
                        Anchor::parse(bytes, la_off + usize::from(rel))
                    });
                }
                comps.push(row);
            }
            lig_records.push(comps);
        }

        Some(Self {
            mark_cov,
            lig_cov,
            mark_records,
            lig_records,
        })
    }

    fn apply(&self, gdef: &GdefData, buf: &mut [GlyphSlot], at: usize) -> Option<usize> {
        let m_idx = usize::from(self.mark_cov.get(buf[at].gid)?);
        let (m_cls, m_anc) = *self.mark_records.get(m_idx)?;
        let mut prev = at;
        let lig_idx = loop {
            if prev == 0 {
                return None;
            }
            prev -= 1;
            if gdef.glyph_class(buf[prev].gid) != 3 {
                break prev;
            }
        };
        let l_idx = usize::from(self.lig_cov.get(buf[lig_idx].gid)?);
        let comps = self.lig_records.get(l_idx)?;
        let comp = usize::from(buf[at].lig_comp).min(comps.len().saturating_sub(1));
        let b_anc = comps.get(comp)?.get(usize::from(m_cls))?.as_ref()?;
        buf[at].x_offset = i32::from(b_anc.x) - i32::from(m_anc.x);
        buf[at].y_offset = i32::from(b_anc.y) - i32::from(m_anc.y);
        buf[at].x_advance = 0;
        buf[at].attach_to = Some(lig_idx);
        Some(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arabic_joining_assigns_contextual_masks_across_transparent_marks() {
        let chars = [
            ('\u{0645}', 0), // Meem (Dual)
            ('\u{064E}', 0), // Fatha (Transparent)
            ('\u{0631}', 1), // Reh (Right)
            ('\u{0628}', 2), // Beh (Dual)
            ('\u{0627}', 3), // Alef (Right)
        ];
        let masks = compute_joining_masks(&chars);
        assert_ne!(masks[0] & MASK_INIT, 0, "Meem before Reh takes init");
        assert_ne!(masks[2] & MASK_FINA, 0, "Reh after Meem takes fina");
        assert_ne!(masks[3] & MASK_INIT, 0, "Beh after Reh takes init");
        assert_ne!(masks[4] & MASK_FINA, 0, "Alef after Beh takes fina");
    }

    #[test]
    fn synthetic_gsub_ligature_and_gpos_mark_anchor_work_end_to_end() {
        // Build tiny synthetic GDEF + GSUB (Type 4 ligature gid 10+11 -> gid 50) +
        // GPOS (Type 2 pair kern + Type 4 mark anchor).
        let mut gdef = Vec::new();
        gdef.extend_from_slice(&1u16.to_be_bytes());
        gdef.extend_from_slice(&0u16.to_be_bytes());
        gdef.extend_from_slice(&12u16.to_be_bytes()); // GlyphClassDef offset
        gdef.extend_from_slice(&[0u8; 6]);
        // ClassDef Format 2: gid 10..=11 -> class 1 (Base), gid 20..=20 -> class 3 (Mark), gid 50..=50 -> class 2 (Lig)
        gdef.extend_from_slice(&2u16.to_be_bytes());
        gdef.extend_from_slice(&3u16.to_be_bytes());
        for (s, e, c) in [(10u16, 11u16, 1u16), (20, 20, 3), (50, 50, 2)] {
            gdef.extend_from_slice(&s.to_be_bytes());
            gdef.extend_from_slice(&e.to_be_bytes());
            gdef.extend_from_slice(&c.to_be_bytes());
        }

        // GSUB: ScriptList(@10), FeatureList(@12), LookupList(@26)
        let mut gsub = Vec::new();
        gsub.extend_from_slice(&1u16.to_be_bytes()); // major
        gsub.extend_from_slice(&0u16.to_be_bytes()); // minor
        gsub.extend_from_slice(&10u16.to_be_bytes()); // ScriptList
        gsub.extend_from_slice(&12u16.to_be_bytes()); // FeatureList
        gsub.extend_from_slice(&26u16.to_be_bytes()); // LookupList
        gsub.extend_from_slice(&0u16.to_be_bytes()); // ScriptList count=0
                                                     // FeatureList @12: 1 feature "liga" -> Feature @8 (rel=8 -> abs 20)
        gsub.extend_from_slice(&1u16.to_be_bytes());
        gsub.extend_from_slice(b"liga");
        gsub.extend_from_slice(&8u16.to_be_bytes());
        // Feature @20: featureParams=0, lookupCount=1, lookupIndex=0
        gsub.extend_from_slice(&0u16.to_be_bytes());
        gsub.extend_from_slice(&1u16.to_be_bytes());
        gsub.extend_from_slice(&0u16.to_be_bytes());
        // LookupList @26: 1 lookup -> Lookup @4 (abs 30)
        gsub.extend_from_slice(&1u16.to_be_bytes());
        gsub.extend_from_slice(&4u16.to_be_bytes());
        // Lookup @30: type=4 (Ligature), flag=0x0008 (IgnoreMarks), subtableCount=1, sub_off=8 (abs 38)
        gsub.extend_from_slice(&4u16.to_be_bytes());
        gsub.extend_from_slice(&0x0008u16.to_be_bytes());
        gsub.extend_from_slice(&1u16.to_be_bytes());
        gsub.extend_from_slice(&8u16.to_be_bytes());
        // LigatureSubst @38: fmt=1, cov=8 (abs 46), setCount=1, set0=14 (abs 52)
        gsub.extend_from_slice(&1u16.to_be_bytes());
        gsub.extend_from_slice(&8u16.to_be_bytes());
        gsub.extend_from_slice(&1u16.to_be_bytes());
        gsub.extend_from_slice(&14u16.to_be_bytes());
        // Cov @46: fmt=1, cnt=1, gid=[10]
        gsub.extend_from_slice(&1u16.to_be_bytes());
        gsub.extend_from_slice(&1u16.to_be_bytes());
        gsub.extend_from_slice(&10u16.to_be_bytes());
        // LigatureSet @52: count=1, lig0=4 (abs 56)
        gsub.extend_from_slice(&1u16.to_be_bytes());
        gsub.extend_from_slice(&4u16.to_be_bytes());
        // Ligature @56: ligGlyph=50, compCount=2, comp1=11
        gsub.extend_from_slice(&50u16.to_be_bytes());
        gsub.extend_from_slice(&2u16.to_be_bytes());
        gsub.extend_from_slice(&11u16.to_be_bytes());

        // GPOS: ScriptList(@10), FeatureList(@12), LookupList(@26) -> MarkBase for gid 20 onto gid 10
        let mut gpos = Vec::new();
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&10u16.to_be_bytes());
        gpos.extend_from_slice(&12u16.to_be_bytes());
        gpos.extend_from_slice(&26u16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes()); // ScriptList count=0
        gpos.extend_from_slice(&1u16.to_be_bytes()); // FeatureList: 1 feature "mark"
        gpos.extend_from_slice(b"mark");
        gpos.extend_from_slice(&8u16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&1u16.to_be_bytes()); // LookupList
        gpos.extend_from_slice(&4u16.to_be_bytes());
        // Lookup @30: type=4 (MarkToBase), flag=0, scnt=1, sub=8 (abs 38)
        gpos.extend_from_slice(&4u16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&8u16.to_be_bytes());
        // MarkBasePos @38 (len=12 header): fmt=1, mcov=12(50), bcov=18(56), classCnt=1, marr=24(62), barr=36(74)
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&12u16.to_be_bytes());
        gpos.extend_from_slice(&18u16.to_be_bytes());
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&24u16.to_be_bytes());
        gpos.extend_from_slice(&36u16.to_be_bytes());
        // MarkCov @50: [20]
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&20u16.to_be_bytes());
        // BaseCov @56: [10]
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&10u16.to_be_bytes());
        // MarkArray @62: cnt=1, class=0, anc_rel=6(abs 68) -> Anchor(100, 50) (ends @74)
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&6u16.to_be_bytes());
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&100i16.to_be_bytes());
        gpos.extend_from_slice(&50i16.to_be_bytes());
        // BaseArray @74: cnt=1, anc_rel=4(abs 78) -> Anchor(350, 700)
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&4u16.to_be_bytes());
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&350i16.to_be_bytes());
        gpos.extend_from_slice(&700i16.to_be_bytes());

        let tables = ShapingTables::parse(Some(&gdef), Some(&gsub), Some(&gpos));
        let glyph_map = |ch: char| match ch {
            'f' => 10,
            'i' => 11,
            '\u{0301}' => 20,
            _ => 0,
        };
        let adv_map = |gid: u16| match gid {
            10 => 400,
            11 => 300,
            20 => 250,
            50 => 650,
            _ => 500,
        };

        // 1. Ligature f + i -> gid 50 (advance 650, span 2)
        let lig = tables.shape(&["f", "i"], glyph_map, adv_map);
        assert_eq!(lig.len(), 1);
        assert_eq!(lig[0].glyph_id, 50);
        assert_eq!(lig[0].cluster_span, 2);
        assert_eq!(lig[0].x_advance, 650);

        // 2. Mark placement f + combining acute -> mark x_advance=0, x_offset = (350-100) - 400 = -150, y_offset = 700-50 = 650
        let marked = tables.shape(&["f\u{0301}"], glyph_map, adv_map);
        assert_eq!(marked.len(), 2);
        assert_eq!(marked[0].x_advance, 400);
        assert_eq!(marked[1].x_advance, 0);
        assert_eq!(marked[1].x_offset, -150);
        assert_eq!(marked[1].y_offset, 650);
    }

    #[test]
    fn stacked_mark_to_mark_does_not_double_subtract_base_advance() {
        let mut buf = vec![
            GlyphSlot {
                gid: 10,
                cluster: 0,
                cluster_span: 1,
                mask: MASK_GLOBAL,
                x_advance: 500,
                x_offset: 0,
                y_offset: 0,
                attach_to: None,
                lig_comp: 0,
                lig_id: 0,
            },
            GlyphSlot {
                gid: 20,
                cluster: 0,
                cluster_span: 1,
                mask: MASK_GLOBAL,
                x_advance: 0,
                x_offset: 250, // b1(350) - m1(100)
                y_offset: 600,
                attach_to: Some(0),
                lig_comp: 0,
                lig_id: 0,
            },
            GlyphSlot {
                gid: 21,
                cluster: 0,
                cluster_span: 1,
                mask: MASK_GLOBAL,
                x_advance: 0,
                x_offset: 20, // b2(120) - m2(100)
                y_offset: 150,
                attach_to: Some(1),
                lig_comp: 0,
                lig_id: 0,
            },
        ];
        resolve_attachments(&mut buf);
        // mark1 sits at base_origin + 250 = pen(500) - 250
        assert_eq!(buf[1].x_offset, -250);
        assert_eq!(buf[1].y_offset, 600);
        // mark2 sits at base_origin + 250 + 20 = pen(500) - 230 (NOT pen - 730!)
        assert_eq!(buf[2].x_offset, -230);
        assert_eq!(buf[2].y_offset, 750);
    }

    #[test]
    fn resolve_attachments_propagates_base_offset_and_ligature_marks() {
        let mut buf = vec![
            GlyphSlot {
                gid: 10,
                cluster: 0,
                cluster_span: 1,
                mask: MASK_GLOBAL,
                x_advance: 500,
                x_offset: 30,
                y_offset: 40,
                attach_to: None,
                lig_comp: 0,
                lig_id: 0,
            },
            GlyphSlot {
                gid: 20,
                cluster: 0,
                cluster_span: 1,
                mask: MASK_GLOBAL,
                x_advance: 0,
                x_offset: 100,
                y_offset: 200,
                attach_to: Some(0),
                lig_comp: 0,
                lig_id: 0,
            },
        ];
        resolve_attachments(&mut buf);
        assert_eq!(buf[1].x_offset, 30 + 100 - 500);
        assert_eq!(buf[1].y_offset, 240);

        let gdef = GdefData {
            glyph_class: ClassDef::Ranges(vec![(10, 11, 1), (20, 21, 3)]),
            mark_attach_class: ClassDef::Empty,
            mark_glyph_sets: Vec::new(),
        };
        let lig = LigatureSubst {
            cov: Coverage::List(vec![10]),
            sets: vec![vec![LigatureRule {
                lig_glyph: 50,
                components: vec![11],
            }]],
        };
        let mut lbuf = vec![
            GlyphSlot {
                gid: 10,
                cluster: 0,
                cluster_span: 1,
                mask: MASK_GLOBAL,
                x_advance: 400,
                x_offset: 0,
                y_offset: 0,
                attach_to: None,
                lig_comp: 0,
                lig_id: 0,
            },
            GlyphSlot {
                gid: 20,
                cluster: 0,
                cluster_span: 1,
                mask: MASK_GLOBAL,
                x_advance: 0,
                x_offset: 0,
                y_offset: 0,
                attach_to: None,
                lig_comp: 0,
                lig_id: 0,
            },
            GlyphSlot {
                gid: 11,
                cluster: 1,
                cluster_span: 1,
                mask: MASK_GLOBAL,
                x_advance: 400,
                x_offset: 0,
                y_offset: 0,
                attach_to: None,
                lig_comp: 0,
                lig_id: 0,
            },
            GlyphSlot {
                gid: 21,
                cluster: 1,
                cluster_span: 1,
                mask: MASK_GLOBAL,
                x_advance: 0,
                x_offset: 0,
                y_offset: 0,
                attach_to: None,
                lig_comp: 0,
                lig_id: 0,
            },
        ];
        assert_eq!(
            lig.apply(&gdef, &mut lbuf, 0, FLAG_IGNORE_MARKS, None),
            Some(1)
        );
        assert_eq!(lbuf.len(), 3);
        assert_eq!(lbuf[0].gid, 50);
        assert_eq!(lbuf[1].lig_comp, 0);
        assert_eq!(lbuf[2].lig_comp, 1);
        assert_eq!(lbuf[1].lig_id, lbuf[0].lig_id);
        assert_eq!(lbuf[2].lig_id, lbuf[0].lig_id);
    }
}
