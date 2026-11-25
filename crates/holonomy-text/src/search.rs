//! Streaming encrypted search across Spine segments without widening the resident set (Block 7).
//!
//! Finding or replacing text across a 30 M-word (`180 MB`) `.wavefunction` document must never
//! decrypt the document into memory (`L-2` / `EYES_ONLY_LAW`). [`find_streaming`] walks the
//! document's leaf spans in bounded byte windows, retaining one loaded segment in a [`Zeroizing`]
//! buffer plus bounded overlap and seam buffers so matches straddling slab or segment cuts are
//! found without touching [`crate::ResidentSet`]. A source may temporarily hold additional copies.

use zeroize::Zeroizing;

use crate::draft::{Draft, LiveSpan};
use crate::formatting::{DocStyleSpan, StyleMap};
use crate::residency::SegmentSource;
use crate::slab::SegmentId;
use crate::source::{Location, OpenSources};
use crate::spine::Spine;

/// Byte-window stride used to walk `Draft::spans_in`. At most one nonempty sealed span can
/// begin per byte, so a small window also bounds temporary span vectors for fragmented spines.
const SCAN_WINDOW_BYTES: u64 = 4 * 1024;

/// Maximum search needle size. The scanner retains up to two needle-length seam buffers.
pub const MAX_SEARCH_NEEDLE_BYTES: usize = 64 * 1024;
/// Maximum results returned by the collecting API. Use [`visit_streaming`] for dense matches.
pub const MAX_COLLECTED_MATCHES: usize = 16 * 1024;

/// One non-overlapping match `[start, end)` in live document byte coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchMatch {
    /// Inclusive start byte offset in the live document.
    pub start: u64,
    /// Exclusive end byte offset in the live document (`end - start == needle.len()`).
    pub end: u64,
}

/// Count and selected match from a bounded-memory scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchSummary {
    /// Total non-overlapping matches.
    pub count: u64,
    /// Match at the requested zero-based index, or the first match when the index is out of range.
    pub selected: Option<SearchMatch>,
}

/// Errors returned by [`find_streaming`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchError {
    /// The needle would exceed the scanner's bounded seam-buffer budget.
    NeedleTooLong {
        /// The maximum accepted needle length in bytes.
        limit: usize,
    },
    /// The collecting API would exceed its result limit; use the streaming visitor instead.
    MatchLimitExceeded {
        /// The maximum number of collected matches.
        limit: usize,
    },
    /// The match count exceeded the representable range.
    MatchCountOverflow,
    /// Failed to load a sealed segment from the [`SegmentSource`].
    Source {
        /// Segment that failed to open.
        segment: SegmentId,
        /// Underlying source error message.
        message: String,
    },
    /// A leaf referenced an unknown provisional source id.
    UnknownSource {
        /// Provisional id missing from [`OpenSources`].
        source: SegmentId,
    },
    /// A leaf's byte slice exceeded its backing segment or pending buffer length.
    SpanOutOfBounds {
        /// Backing source/segment id.
        segment: SegmentId,
        /// Requested start byte inside the segment.
        start: u32,
        /// Requested end byte inside the segment.
        end: u32,
        /// Actual byte length of the segment.
        actual_len: usize,
    },
}

impl std::fmt::Display for SearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NeedleTooLong { limit } => write!(f, "search needle exceeds {limit} bytes"),
            Self::MatchLimitExceeded { limit } => {
                write!(f, "more than {limit} matches exceed the collection limit")
            }
            Self::MatchCountOverflow => write!(f, "search match count overflow"),
            Self::Source { segment, message } => {
                write!(
                    f,
                    "failed to load segment {segment} during search: {message}"
                )
            }
            Self::UnknownSource { source } => {
                write!(f, "search encountered unknown provisional source {source}")
            }
            Self::SpanOutOfBounds {
                segment,
                start,
                end,
                actual_len,
            } => write!(
                f,
                "slab {start}..{end} exceeds {actual_len}-byte segment {segment}"
            ),
        }
    }
}

impl std::error::Error for SearchError {}

/// Collects non-overlapping matches, refusing results above [`MAX_COLLECTED_MATCHES`].
/// Use [`visit_streaming`] to process dense result sets without accumulating them.
/// An empty needle returns an empty vector.
pub fn find_streaming<R: SegmentSource>(
    spine: &Spine,
    draft: &Draft,
    sources: &OpenSources,
    pending: &[u8],
    needle: &[u8],
    source: &mut R,
) -> Result<Vec<SearchMatch>, SearchError> {
    let mut matches = Vec::new();
    let mut overflow = false;
    visit_streaming(spine, draft, sources, pending, needle, source, |hit| {
        if matches.len() == MAX_COLLECTED_MATCHES {
            overflow = true;
            return false;
        }
        matches.push(hit);
        true
    })?;
    if overflow {
        Err(SearchError::MatchLimitExceeded {
            limit: MAX_COLLECTED_MATCHES,
        })
    } else {
        Ok(matches)
    }
}

/// Visits non-overlapping matches in document order without collecting them. Return `false` from
/// `visit` to stop early. The visitor must not mutate the document being scanned. One sealed
/// plaintext segment and bounded seam buffers are held at a time; the visitor owns any memory it
/// chooses to allocate. An empty needle visits no matches.
pub fn visit_streaming<R: SegmentSource>(
    spine: &Spine,
    draft: &Draft,
    sources: &OpenSources,
    pending: &[u8],
    needle: &[u8],
    source: &mut R,
    mut visit: impl FnMut(SearchMatch) -> bool,
) -> Result<(), SearchError> {
    if needle.len() > MAX_SEARCH_NEEDLE_BYTES {
        return Err(SearchError::NeedleTooLong {
            limit: MAX_SEARCH_NEEDLE_BYTES,
        });
    }
    if needle.is_empty() {
        return Ok(());
    }
    let total_len = draft.document_len(spine);
    if (needle.len() as u64) > total_len {
        return Ok(());
    }

    let m = needle.len();
    let keep = m - 1;
    let first_byte = needle[0];
    let last_byte = needle[m - 1];

    let mut cached: Option<(SegmentId, Zeroizing<Vec<u8>>)> = None;
    let mut overlap: Zeroizing<Vec<u8>> = Zeroizing::new(Vec::with_capacity(keep));
    let mut seam: Zeroizing<Vec<u8>> = Zeroizing::new(Vec::with_capacity(2 * m));
    let mut doc_pos = 0u64;
    let mut next_allowed = 0u64;
    let mut window_start = 0u64;

    while window_start < total_len {
        let window_end = window_start
            .saturating_add(SCAN_WINDOW_BYTES)
            .min(total_len);
        let spans = draft.spans_in(spine, window_start, window_end);
        if spans.is_empty() {
            break;
        }
        for span in spans {
            let (span_offset, span_len) = match span {
                LiveSpan::Sealed { offset, slab } => (offset, u64::from(slab.len())),
                LiveSpan::Draft { offset, len, .. } => (offset, len),
            };
            let span_end = span_offset.saturating_add(span_len);
            if span_end <= doc_pos {
                continue;
            }
            let skip = usize::try_from(doc_pos.saturating_sub(span_offset)).unwrap_or(0);

            let chunk: &[u8] = match span {
                LiveSpan::Draft { slab, len, .. } => {
                    let take_end = usize::try_from(len).unwrap_or(usize::MAX);
                    if take_end > pending.len() || skip > take_end {
                        return Err(SearchError::SpanOutOfBounds {
                            segment: slab.segment(),
                            start: skip as u32,
                            end: len as u32,
                            actual_len: pending.len(),
                        });
                    }
                    &pending[skip..take_end]
                }
                LiveSpan::Sealed { slab, .. } => match sources.locate(slab.segment()) {
                    Location::Open { .. } => {
                        let start = (slab.start() as usize).saturating_add(skip);
                        let end = slab.end() as usize;
                        if end > pending.len() || start > end {
                            return Err(SearchError::SpanOutOfBounds {
                                segment: slab.segment(),
                                start: slab.start(),
                                end: slab.end(),
                                actual_len: pending.len(),
                            });
                        }
                        &pending[start..end]
                    }
                    Location::Unknown => {
                        return Err(SearchError::UnknownSource {
                            source: slab.segment(),
                        });
                    }
                    Location::Sealed { segment } => {
                        let reuse = cached.as_ref().is_some_and(|(id, _)| *id == segment);
                        if !reuse {
                            // Drop and zeroize the previous segment before loading the next one.
                            drop(cached.take());
                            let bytes = source
                                .load(segment)
                                .map_err(|message| SearchError::Source { segment, message })?;
                            cached = Some((segment, Zeroizing::new(bytes)));
                        }
                        let seg_bytes = &cached.as_ref().expect("just populated").1;
                        let start = (slab.start() as usize).saturating_add(skip);
                        let end = slab.end() as usize;
                        if end > seg_bytes.len() || start > end {
                            return Err(SearchError::SpanOutOfBounds {
                                segment,
                                start: slab.start(),
                                end: slab.end(),
                                actual_len: seg_bytes.len(),
                            });
                        }
                        &seg_bytes[start..end]
                    }
                },
            };

            if chunk.is_empty() {
                continue;
            }

            if !overlap.is_empty() {
                let head_len = keep.min(chunk.len());
                seam.clear();
                seam.extend_from_slice(&overlap);
                seam.extend_from_slice(&chunk[..head_len]);
                let seam_base = doc_pos - overlap.len() as u64;
                let mut idx = 0usize;
                while idx < overlap.len() && idx + m <= seam.len() {
                    let cand = seam_base + idx as u64;
                    if cand >= next_allowed
                        && seam[idx] == first_byte
                        && seam[idx + m - 1] == last_byte
                        && &seam[idx..idx + m] == needle
                    {
                        let end = cand + m as u64;
                        if !visit(SearchMatch { start: cand, end }) {
                            return Ok(());
                        }
                        next_allowed = end;
                        idx += m;
                    } else {
                        idx += 1;
                    }
                }
            }

            let mut idx =
                usize::try_from(next_allowed.saturating_sub(doc_pos)).unwrap_or(chunk.len());
            if chunk.len() >= m {
                let last_start = chunk.len() + 1 - m;
                while idx < last_start {
                    let Some(rel) = chunk[idx..last_start].iter().position(|&b| b == first_byte)
                    else {
                        break;
                    };
                    idx += rel;
                    if chunk[idx + m - 1] == last_byte && &chunk[idx..idx + m] == needle {
                        let start = doc_pos + idx as u64;
                        let end = start + m as u64;
                        if !visit(SearchMatch { start, end }) {
                            return Ok(());
                        }
                        idx += m;
                        next_allowed = end;
                    } else {
                        idx += 1;
                    }
                }
            }

            if keep > 0 {
                if chunk.len() >= keep {
                    overlap.clear();
                    overlap.extend_from_slice(&chunk[chunk.len() - keep..]);
                } else {
                    let total = overlap.len() + chunk.len();
                    if total > keep {
                        overlap.drain(..total - keep);
                    }
                    overlap.extend_from_slice(chunk);
                }
            }

            doc_pos += chunk.len() as u64;
        }
        window_start = doc_pos.max(window_end);
    }

    Ok(())
}

/// Output summary returned by [`stream_replace_chunks`].
#[derive(Debug, Clone)]
pub struct StreamReplaceResult {
    /// Total non-overlapping matches replaced across the document.
    pub count: usize,
    /// Byte offset of the first replaced match in original coordinates, if any.
    pub first_match_start: Option<u64>,
    /// Byte offset immediately after the last emitted replacement (for caret placement).
    pub final_caret: u64,
    /// Transformed rich-text styles aligned with the streamed replacement output.
    pub styles: StyleMap,
}

/// Errors returned by [`stream_replace_chunks`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamReplaceError {
    /// Failed reading or validating source document segments.
    Search(SearchError),
    /// Failed publishing a chunk to the streaming sink.
    Sink(String),
    /// `needle` or `replacement` was not valid UTF-8, or a boundary split invalid UTF-8.
    InvalidUtf8,
    /// Arithmetic overflow while computing replacement offsets.
    OffsetOverflow,
}

impl std::fmt::Display for StreamReplaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Search(error) => error.fmt(f),
            Self::Sink(reason) => write!(f, "stream publish failed: {reason}"),
            Self::InvalidUtf8 => f.write_str("invalid UTF-8 in streaming replace"),
            Self::OffsetOverflow => f.write_str("streaming replacement offset overflow"),
        }
    }
}

impl std::error::Error for StreamReplaceError {}

impl From<SearchError> for StreamReplaceError {
    fn from(error: SearchError) -> Self {
        Self::Search(error)
    }
}

struct StyleCursor<'a> {
    orig_runs: &'a [DocStyleSpan],
    links: Vec<String>,
    run_idx: usize,
    cur_src: u64,
    cur_dst: u64,
    out_runs: Vec<DocStyleSpan>,
}

impl<'a> StyleCursor<'a> {
    fn new(styles: &'a StyleMap) -> Self {
        Self {
            orig_runs: styles.runs(),
            links: styles.links().to_vec(),
            run_idx: 0,
            cur_src: 0,
            cur_dst: 0,
            out_runs: Vec::new(),
        }
    }

    fn push_coalesced(&mut self, span: DocStyleSpan) {
        if span.start >= span.end || span.style.is_default() {
            return;
        }
        if let Some(last) = self.out_runs.last_mut() {
            if last.end == span.start && last.style == span.style {
                last.end = span.end;
                return;
            }
        }
        self.out_runs.push(span);
    }

    fn emit_kept(&mut self, len: u64) -> Result<(), StreamReplaceError> {
        if len == 0 {
            return Ok(());
        }
        let src_start = self.cur_src;
        let src_end = src_start
            .checked_add(len)
            .ok_or(StreamReplaceError::OffsetOverflow)?;
        if !self.orig_runs.is_empty() {
            while self.run_idx < self.orig_runs.len()
                && self.orig_runs[self.run_idx].end <= src_start
            {
                self.run_idx += 1;
            }
            while self.run_idx < self.orig_runs.len()
                && self.orig_runs[self.run_idx].start < src_end
            {
                let run = &self.orig_runs[self.run_idx];
                let ov_start = run.start.max(src_start);
                let ov_end = run.end.min(src_end);
                self.push_coalesced(DocStyleSpan {
                    start: self.cur_dst + (ov_start - src_start),
                    end: self.cur_dst + (ov_end - src_start),
                    style: run.style,
                });
                if run.end <= src_end {
                    self.run_idx += 1;
                } else {
                    break;
                }
            }
        }
        self.cur_src = src_end;
        self.cur_dst = self
            .cur_dst
            .checked_add(len)
            .ok_or(StreamReplaceError::OffsetOverflow)?;
        Ok(())
    }

    fn emit_match(&mut self, needle_len: u64, rep_len: u64) -> Result<(), StreamReplaceError> {
        let src_start = self.cur_src;
        let src_end = src_start
            .checked_add(needle_len)
            .ok_or(StreamReplaceError::OffsetOverflow)?;
        if !self.orig_runs.is_empty() {
            while self.run_idx < self.orig_runs.len()
                && self.orig_runs[self.run_idx].end <= src_start
            {
                self.run_idx += 1;
            }
            if rep_len > 0 {
                if let Some(run) = self.orig_runs.get(self.run_idx) {
                    if run.start <= src_start && run.end >= src_end {
                        let dst_end = self
                            .cur_dst
                            .checked_add(rep_len)
                            .ok_or(StreamReplaceError::OffsetOverflow)?;
                        self.push_coalesced(DocStyleSpan {
                            start: self.cur_dst,
                            end: dst_end,
                            style: run.style,
                        });
                    }
                }
            }
            while self.run_idx < self.orig_runs.len() && self.orig_runs[self.run_idx].end <= src_end
            {
                self.run_idx += 1;
            }
        }
        self.cur_src = src_end;
        self.cur_dst = self
            .cur_dst
            .checked_add(rep_len)
            .ok_or(StreamReplaceError::OffsetOverflow)?;
        Ok(())
    }

    fn finish(self) -> StyleMap {
        if self.out_runs.is_empty() {
            StyleMap::empty()
        } else {
            StyleMap::normalize_runs(self.out_runs, self.links)
        }
    }
}

fn drain_output_chunks(
    out_buf: &mut Zeroizing<Vec<u8>>,
    max_chunk_bytes: usize,
    flush_all: bool,
    emit_chunk: &mut impl FnMut(&[u8]) -> Result<(), String>,
) -> Result<(), StreamReplaceError> {
    loop {
        if out_buf.is_empty() {
            break;
        }
        if !flush_all && out_buf.len() < max_chunk_bytes {
            break;
        }
        let mut take = out_buf.len().min(max_chunk_bytes);
        if take == 0 {
            break;
        }
        let tail_start = take.saturating_sub(4);
        let check_from = if let Some(rel) = (0..=tail_start)
            .rev()
            .take(4)
            .find(|&pos| (out_buf[pos] & 0xC0) != 0x80)
        {
            rel
        } else {
            tail_start
        };
        if let Err(err) = core::str::from_utf8(&out_buf[check_from..take]) {
            let valid = check_from + err.valid_up_to();
            if valid == 0 {
                if flush_all {
                    return Err(StreamReplaceError::InvalidUtf8);
                }
                break;
            }
            if !flush_all && err.error_len().is_some() {
                return Err(StreamReplaceError::InvalidUtf8);
            }
            if flush_all {
                return Err(StreamReplaceError::InvalidUtf8);
            }
            take = valid;
        }
        emit_chunk(&out_buf[..take]).map_err(StreamReplaceError::Sink)?;
        out_buf.drain(..take);
    }
    Ok(())
}

/// Streams a flushed [`Spine`], replaces every non-overlapping occurrence of `needle` with
/// `replacement` in bounded memory (independent of match count), and invokes `emit_chunk` for each
/// `<= max_chunk_bytes` valid UTF-8 output chunk (`Block 7`, `OQ-32`).
pub fn stream_replace_chunks<R: SegmentSource>(
    spine: &Spine,
    sources: &OpenSources,
    needle: &[u8],
    replacement: &[u8],
    max_chunk_bytes: usize,
    source: &mut R,
    mut emit_chunk: impl FnMut(&[u8]) -> Result<(), String>,
) -> Result<StreamReplaceResult, StreamReplaceError> {
    if core::str::from_utf8(needle).is_err() || core::str::from_utf8(replacement).is_err() {
        return Err(StreamReplaceError::InvalidUtf8);
    }
    if needle.len() > MAX_SEARCH_NEEDLE_BYTES {
        return Err(SearchError::NeedleTooLong {
            limit: MAX_SEARCH_NEEDLE_BYTES,
        }
        .into());
    }
    if needle.is_empty() || max_chunk_bytes < 4 {
        return Ok(StreamReplaceResult {
            count: 0,
            first_match_start: None,
            final_caret: 0,
            styles: spine.styles().clone(),
        });
    }

    let total_len = spine.len();
    let m = needle.len();
    let keep = m - 1;
    let rep_len = replacement.len() as u64;
    let needle_len = m as u64;
    let first_byte = needle[0];
    let last_byte = needle[m - 1];

    let mut cached: Option<(SegmentId, Zeroizing<Vec<u8>>)> = None;
    let mut holdback: Zeroizing<Vec<u8>> =
        Zeroizing::new(Vec::with_capacity(keep + SCAN_WINDOW_BYTES as usize));
    let mut out_buf: Zeroizing<Vec<u8>> =
        Zeroizing::new(Vec::with_capacity(max_chunk_bytes.saturating_add(4096)));
    let mut style_cursor = StyleCursor::new(spine.styles());

    let mut count = 0usize;
    let mut first_match_start: Option<u64> = None;
    let mut final_caret = 0u64;
    let mut doc_pos = 0u64;
    let mut window_start = 0u64;

    while window_start < total_len {
        let window_end = window_start
            .saturating_add(SCAN_WINDOW_BYTES)
            .min(total_len);
        let spans = spine.spans_in(window_start, window_end);
        if spans.is_empty() {
            break;
        }
        for span in spans {
            let span_offset = span.offset();
            let slab = span.slab();
            let span_end = span_offset.saturating_add(u64::from(slab.len()));
            if span_end <= doc_pos {
                continue;
            }
            let skip = usize::try_from(doc_pos.saturating_sub(span_offset)).unwrap_or(0);
            let segment = match sources.locate(slab.segment()) {
                Location::Sealed { segment } => segment,
                Location::Open { .. } | Location::Unknown => {
                    return Err(SearchError::UnknownSource {
                        source: slab.segment(),
                    }
                    .into());
                }
            };
            let reuse = cached.as_ref().is_some_and(|(id, _)| *id == segment);
            if !reuse {
                drop(cached.take());
                let bytes = source
                    .load(segment)
                    .map_err(|message| SearchError::Source { segment, message })?;
                cached = Some((segment, Zeroizing::new(bytes)));
            }
            let seg_bytes = &cached.as_ref().expect("just populated").1;
            let start = (slab.start() as usize).saturating_add(skip);
            let end = slab.end() as usize;
            if end > seg_bytes.len() || start > end {
                return Err(SearchError::SpanOutOfBounds {
                    segment,
                    start: slab.start(),
                    end: slab.end(),
                    actual_len: seg_bytes.len(),
                }
                .into());
            }

            let mut slice = &seg_bytes[start..end];
            doc_pos = doc_pos.saturating_add(slice.len() as u64);

            // Feed in bounded sub-slices so `holdback` and `out_buf` stay O(max_chunk_bytes).
            while !slice.is_empty() {
                let take_in = slice.len().min(SCAN_WINDOW_BYTES as usize);
                holdback.extend_from_slice(&slice[..take_in]);
                slice = &slice[take_in..];

                let mut scan = 0usize;
                let mut emitted_up_to = 0usize;
                while scan + m <= holdback.len() {
                    if holdback[scan] == first_byte
                        && holdback[scan + m - 1] == last_byte
                        && &holdback[scan..scan + m] == needle
                    {
                        let kept_len = scan - emitted_up_to;
                        if kept_len > 0 {
                            out_buf.extend_from_slice(&holdback[emitted_up_to..scan]);
                            style_cursor.emit_kept(kept_len as u64)?;
                            drain_output_chunks(
                                &mut out_buf,
                                max_chunk_bytes,
                                false,
                                &mut emit_chunk,
                            )?;
                        }
                        if first_match_start.is_none() {
                            first_match_start = Some(style_cursor.cur_src);
                        }
                        count = count
                            .checked_add(1)
                            .ok_or(SearchError::MatchCountOverflow)?;
                        out_buf.extend_from_slice(replacement);
                        style_cursor.emit_match(needle_len, rep_len)?;
                        final_caret = style_cursor.cur_dst;
                        drain_output_chunks(&mut out_buf, max_chunk_bytes, false, &mut emit_chunk)?;
                        scan += m;
                        emitted_up_to = scan;
                    } else {
                        scan += 1;
                    }
                }

                let keep_from = if emitted_up_to == holdback.len() || keep == 0 {
                    holdback.len()
                } else {
                    emitted_up_to.max(holdback.len().saturating_sub(keep))
                };
                if keep_from > emitted_up_to {
                    out_buf.extend_from_slice(&holdback[emitted_up_to..keep_from]);
                    style_cursor.emit_kept((keep_from - emitted_up_to) as u64)?;
                    drain_output_chunks(&mut out_buf, max_chunk_bytes, false, &mut emit_chunk)?;
                }
                holdback.drain(..keep_from);
            }
        }
        window_start = doc_pos.max(window_end);
    }

    if !holdback.is_empty() {
        out_buf.extend_from_slice(&holdback);
        style_cursor.emit_kept(holdback.len() as u64)?;
        holdback.clear();
    }
    drain_output_chunks(&mut out_buf, max_chunk_bytes, true, &mut emit_chunk)?;

    Ok(StreamReplaceResult {
        count,
        first_match_start,
        final_caret,
        styles: style_cursor.finish(),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::slab::Slab;

    struct MapSource(BTreeMap<SegmentId, Vec<u8>>);

    impl SegmentSource for MapSource {
        fn load(&mut self, segment: SegmentId) -> Result<Vec<u8>, String> {
            self.0
                .get(&segment)
                .cloned()
                .ok_or_else(|| format!("missing segment {segment}"))
        }
    }

    #[test]
    fn dense_matches_can_be_visited_without_collecting_and_collection_fails_closed() {
        let id = SegmentId::new(0);
        let size = MAX_COLLECTED_MATCHES + 100;
        let mut store = MapSource(BTreeMap::from([(id, vec![b'x'; size])]));
        let spine = Spine::from_slabs(vec![Slab::new(id, 0, size as u32)]);
        let draft = Draft::new();
        let sources = OpenSources::new();
        let mut count = 0usize;
        visit_streaming(&spine, &draft, &sources, &[], b"x", &mut store, |hit| {
            assert_eq!(hit.start, count as u64);
            count += 1;
            true
        })
        .unwrap();
        assert_eq!(count, size);
        assert_eq!(
            find_streaming(&spine, &draft, &sources, &[], b"x", &mut store),
            Err(SearchError::MatchLimitExceeded {
                limit: MAX_COLLECTED_MATCHES
            })
        );
        let mut visited = 0;
        visit_streaming(&spine, &draft, &sources, &[], b"x", &mut store, |_| {
            visited += 1;
            false
        })
        .unwrap();
        assert_eq!(visited, 1);
        let huge = vec![b'x'; MAX_SEARCH_NEEDLE_BYTES + 1];
        assert_eq!(
            find_streaming(&spine, &draft, &sources, &[], &huge, &mut store),
            Err(SearchError::NeedleTooLong {
                limit: MAX_SEARCH_NEEDLE_BYTES
            })
        );
    }

    #[test]
    fn visitor_walks_many_one_byte_slabs_without_missing_window_boundaries() {
        let count = 10_000;
        let mut entries = BTreeMap::new();
        let mut slabs = Vec::new();
        for index in 0..count {
            let id = SegmentId::new(index);
            entries.insert(id, vec![b'x']);
            slabs.push(Slab::new(id, 0, 1));
        }
        let spine = Spine::from_slabs(slabs);
        let mut store = MapSource(entries);
        let mut seen = 0u64;
        visit_streaming(
            &spine,
            &Draft::new(),
            &OpenSources::new(),
            &[],
            b"x",
            &mut store,
            |hit| {
                assert_eq!(hit.start, seen);
                seen += 1;
                true
            },
        )
        .unwrap();
        assert_eq!(seen, count);
    }

    #[test]
    fn visitor_finds_matches_across_scan_windows_and_segment_boundaries() {
        let left = SegmentId::new(0);
        let right = SegmentId::new(1);
        let mut first = vec![b'z'; 8192];
        first[4094..4096].copy_from_slice(b"ab");
        first[4096..4098].copy_from_slice(b"cd");
        first[8190..8192].copy_from_slice(b"ab");
        let mut store = MapSource(BTreeMap::from([(left, first), (right, b"cd!".to_vec())]));
        let spine = Spine::from_slabs(vec![Slab::new(left, 0, 8192), Slab::new(right, 0, 3)]);
        let mut hits = Vec::new();
        visit_streaming(
            &spine,
            &Draft::new(),
            &OpenSources::new(),
            &[],
            b"abcd",
            &mut store,
            |hit| {
                hits.push(hit);
                true
            },
        )
        .unwrap();
        assert_eq!(
            hits,
            vec![
                SearchMatch {
                    start: 4094,
                    end: 4098
                },
                SearchMatch {
                    start: 8190,
                    end: 8194
                }
            ]
        );
    }

    #[test]
    fn streaming_search_finds_matches_across_multi_slab_seams_without_overlap() {
        let mut store = MapSource(BTreeMap::new());
        let pieces: [&[u8]; 5] = [b"xa", b"bc", b"da", b"bc", b"d!"];
        let mut slabs = Vec::new();
        for (i, piece) in pieces.iter().enumerate() {
            let id = SegmentId::new(i as u64);
            store.0.insert(id, piece.to_vec());
            slabs.push(Slab::new(id, 0, piece.len() as u32));
        }
        let spine = Spine::from_slabs(slabs);
        let hits = find_streaming(
            &spine,
            &Draft::new(),
            &OpenSources::new(),
            &[],
            b"abcd",
            &mut store,
        )
        .unwrap();
        assert_eq!(
            hits,
            vec![
                SearchMatch { start: 1, end: 5 },
                SearchMatch { start: 5, end: 9 },
            ]
        );
    }
}
