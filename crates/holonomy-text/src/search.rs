//! Streaming encrypted search across Spine segments without widening the resident set (Block 7).
//!
//! Finding or replacing text across a 30 M-word (`180 MB`) `.wavefunction` document must never
//! decrypt the document into memory (`L-2` / `EYES_ONLY_LAW`). [`find_streaming`] walks the
//! document's leaf spans in bounded byte windows, retaining one loaded segment in a [`Zeroizing`]
//! buffer plus bounded overlap and seam buffers so matches straddling slab or segment cuts are
//! found without touching [`crate::ResidentSet`]. A source may temporarily hold additional copies.

use zeroize::Zeroizing;

use crate::draft::{Draft, LiveSpan};
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
