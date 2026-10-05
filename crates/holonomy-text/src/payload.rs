//! The document payload: what actually traverses chunks 1..N.
//!
//! # The container does not know what this is
//!
//! `Wavefunction::create` and `Wavefunction::write_content` take `&[u8]`; `read_content` returns
//! `Vec<u8>`. `MasterFrame` records `content_len` and the container chops it into 65,520-byte chunks.
//! **That is the entire contract**, and it is why the container format could be frozen while this
//! module was written: nothing outside this file has an opinion about a payload byte, so a payload
//! section can be added without a `FORMAT_VERSION` bump, a `MasterFrame` field, or a section table at
//! the container level. `crates/holonomy-container/tests/commit_then_read.rs` keeps passing unchanged,
//! which is the check that this claim is true rather than merely intended.
//!
//! # Why there was nothing here before
//!
//! Until now the payload *was* the document's UTF-8 text and nothing else, and the other state was
//! recovered rather than stored:
//!
//! | state | recoverable? | how |
//! |---|---|---|
//! | text | trivially | it is the bytes |
//! | style spans | yes | **not** -- `Color` and `Bold` are not in the text, so spans are stored |
//! | tables | partly | `2x3` and `1x6` are the same six separator bytes, so `rows`/`cols`/`col_widths` are stored |
//! | math | yes | `for_each_math_span` re-derives every `$$` span from the bytes |
//! | assets | **no** | the pixels are not in the text, so the catalog is stored |
//!
//! Math is the interesting row: it is derivable, and this module stores it anyway. See
//! [`Payload::decode`] for why that is a cross-check rather than a second source of truth.
//!
//! # The layout
//!
//! ```text
//! [Doc Header 16B]
//! [Text & Spans]
//! [Table & Math States]
//! [Asset Catalog Header: count u32]
//! [Asset: Blake2b 32B | w u16 | h u16 | len u32 | PNG]*
//! ```
//!
//! Two properties make it walkable without a section table, and both are the reason it is laid out
//! this way rather than with a table of section offsets:
//!
//! * **Every variable-length run is preceded by its length.** Counts precede counts; the PNG length
//!   precedes the PNG. So the reader never seeks and never guesses.
//! * **The catalog is last and reads to end of input.** Nothing follows it, so no offset to it is
//!   needed -- which is what lets it be appended to a payload that already existed without moving a
//!   single byte in front of it.
//!
//! All integers are little-endian, matching [`holonomy_container::MasterFrame::encode`], the only
//! other hand-rolled encoder in the workspace. There are no varints: the biggest field is a `u32`
//! byte count and the smallest fixed header is 16 bytes, so a varint would save nothing worth a
//! second encoding to get wrong.

use crate::asset::{scan_anchors, AssetCatalog, AssetError, ANCHOR_BYTES};
use crate::math_span::{for_each_math_span, math_span_count, MathSpan};
use crate::span::{SpanError, SpanMap, TextIntervalSpan};
use crate::table::{TableError, TableSpan};

use core::fmt;

/// Bytes in the doc header.
pub const HEADER_LEN: usize = 16;

/// The magic that starts every payload: `HDOC`.
pub const MAGIC: [u8; 4] = *b"HDOC";

/// The payload format version. Independent of the container's `FORMAT_VERSION`.
///
/// Two version numbers is not redundancy: `FORMAT_VERSION` says how to *seal* and *find* chunks, and
/// the container must not learn a new value because a payload changed. This one says how to read
/// bytes *inside* a chunk, which the container has no business knowing about.
pub const FORMAT: u16 = 1;

/// A payload header carries the asset catalog.
pub const FLAG_ASSETS: u16 = 1 << 0;

/// Every flag bit this version defines. A header with a bit set outside this mask is refused, so a
/// future flag is a version bump rather than a silently ignored field.
pub const FLAG_MASK: u16 = FLAG_ASSETS;

// Byte offsets into the header. Named so the encode and decode halves cannot disagree about which
// byte is which, which is the classic way a hand-rolled header rots.
const OFF_MAGIC: usize = 0;
const OFF_FORMAT: usize = 4;
const OFF_FLAGS: usize = 6;
const OFF_TEXT_LEN: usize = 8;
const OFF_SPAN_COUNT: usize = 12;
const OFF_TABLE_COUNT: usize = 14;

/// Bytes per serialised `TableSpan`: `rows` 2, `cols` 2, `col_widths` 16, `start` 4, `end` 4.
const TABLE_BYTES: usize = 2 + 2 + 2 * TableSpan::MAX_COLS + 4 + 4;

/// Bytes per serialised `MathSpan`: `start` 4, `end` 4, `closed` 1, then 3 bytes of padding.
///
/// The padding is explicit rather than assumed: this is a byte format crossing a chunk boundary, so
/// "the compiler happened to pad it" is not a property that survives being written down and read back
/// by something else.
const MATH_BYTES: usize = 4 + 4 + 1 + 3;

/// Bytes per serialised [`TextIntervalSpan`](crate::TextIntervalSpan): 16, and `size_of` pins it.
const SPAN_BYTES: usize = 16;

/// Why a payload was refused.
///
/// Not `Copy`, because two of its variants carry an upstream error and [`SpanError`] has an
/// `Invariant(String)` arm. `Clone` rather than `Debug`-only because a save path that wants to report
/// an error and carry on with the previous payload should be able to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PayloadError {
    /// Fewer bytes than the header alone needs.
    TooShort {
        /// Bytes available.
        have: usize,
        /// Bytes needed.
        need: usize,
    },
    /// The magic is not [`MAGIC`].
    BadMagic,
    /// A payload format this build does not read.
    UnsupportedFormat {
        /// The format found.
        found: u16,
        /// The format this build reads.
        want: u16,
    },
    /// A flag bit outside [`FLAG_MASK`] is set.
    UnknownFlags {
        /// The flags found.
        found: u16,
    },
    /// The text length runs past the end of the buffer.
    TextOverruns {
        /// Bytes declared.
        declared: u32,
        /// Bytes available.
        have: usize,
    },
    /// A span, table or math record runs past the end of the buffer.
    RecordOverruns {
        /// What ran off the end.
        what: &'static str,
        /// Bytes needed.
        need: usize,
        /// Bytes available.
        have: usize,
    },
    /// The text is not valid UTF-8.
    NotUtf8,
    /// The span map rejected the spans.
    Span(SpanError),
    /// A table span was malformed.
    Table(TableError),
    /// The asset catalog was malformed.
    Asset(AssetError),
    /// The stored math spans disagree with the ones the text implies.
    ///
    /// Math is derivable, so this cannot be caused by an attacker holding the key -- the AEAD tag
    /// already covers the whole payload. It fires on a bug in this file, and it is checked because a
    /// silently-wrong math span is a formula rendered at the wrong place rather than an error.
    MathMismatch {
        /// Spans stored in the payload.
        stored: usize,
        /// Spans the text implies.
        derived: usize,
    },
    /// The right *number* of formula spans are stored, and at least one of them is not the span the
    /// text puts at that offset.
    MathStateMismatch {
        /// Where the span the text implies starts.
        start: u32,
        /// Where it ends.
        end: u32,
        /// Whether the text's span is closed.
        closed: bool,
    },
}

impl fmt::Display for PayloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort { have, need } => {
                write!(f, "payload is {have} bytes: the header needs {need}")
            }
            Self::BadMagic => write!(f, "not a holonomy payload: the magic is wrong"),
            Self::UnsupportedFormat { found, want } => {
                write!(f, "payload format {found}, this build reads {want}")
            }
            Self::UnknownFlags { found } => {
                write!(
                    f,
                    "payload flags {found:#06x} set bits this build does not define"
                )
            }
            Self::TextOverruns { declared, have } => {
                write!(
                    f,
                    "payload declares {declared} bytes of text and holds {have}"
                )
            }
            Self::RecordOverruns { what, need, have } => {
                write!(f, "{what} needs {need} bytes and only {have} are left")
            }
            Self::NotUtf8 => write!(f, "the payload's text is not valid UTF-8"),
            Self::Span(e) => write!(f, "span map: {e}"),
            Self::Table(e) => write!(f, "table: {e}"),
            Self::Asset(e) => write!(f, "asset catalog: {e}"),
            Self::MathMismatch { stored, derived } => write!(
                f,
                "the payload stores {stored} formula spans but its text implies {derived}"
            ),
            Self::MathStateMismatch { start, end, closed } => write!(
                f,
                "the payload's formula record disagrees with the text's span [{start}, {end}), \
                 closed={closed}"
            ),
        }
    }
}

impl std::error::Error for PayloadError {}

impl From<SpanError> for PayloadError {
    fn from(e: SpanError) -> Self {
        Self::Span(e)
    }
}

impl From<TableError> for PayloadError {
    fn from(e: TableError) -> Self {
        Self::Table(e)
    }
}

impl From<AssetError> for PayloadError {
    fn from(e: AssetError) -> Self {
        Self::Asset(e)
    }
}

/// A document read back out of a payload.
#[derive(Debug)]
pub struct Decoded {
    /// The document's UTF-8 text.
    pub text: String,
    /// The style span map.
    pub spans: SpanMap,
    /// Table shapes, in document order.
    pub tables: Vec<TableSpan>,
    /// The image assets, in document order.
    pub assets: AssetCatalog,
}

/// Serialise a document's state into a payload.
///
/// `math` is not a parameter: it is derived from `text` by `for_each_math_span` here, so it cannot be
/// passed in disagreeing with the text. Storing it is a cross-check on this file's own arithmetic,
/// not a way for a caller to inject a formula span that the text does not contain.
pub fn encode(text: &str, spans: &SpanMap, tables: &[TableSpan], assets: &AssetCatalog) -> Vec<u8> {
    // A gap-free map is what every mutator leaves behind, so for a real document this list is never
    // empty. `SpanMap::empty_over` can produce an empty one, though, and an empty list would encode
    // to `span_count == 0` and decode back to *one* plain span -- a round trip that changes the bytes,
    // which is the exact failure the other tests here assert does not happen. So the canonical form
    // is written: one plain span over the text.
    let canonical: &[TextIntervalSpan] = if spans.spans().is_empty() && !text.is_empty() {
        &[TextIntervalSpan::plain(0, text.len() as u32)]
    } else {
        spans.spans()
    };
    let span_list = canonical;
    let mut out = Vec::with_capacity(
        HEADER_LEN
            + text.len()
            + span_list.len() * SPAN_BYTES
            + tables.len() * TABLE_BYTES
            + 4
            + assets.encoded_len(),
    );

    // Header. `u16::try_from(...).unwrap_or(u16::MAX)` cannot fire for a real document: 65,535 spans
    // over an 8 MiB payload is one span per 128 bytes, and the span map's own policy caps the count.
    // It is written as a saturating conversion rather than a debug assert so that a payload
    // written on a lenient build is still *readable* -- it just loses spans, and it says so here.
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&FORMAT.to_le_bytes());
    let flags = if assets.is_empty() { 0 } else { FLAG_ASSETS };
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&u32::try_from(text.len()).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(
        &u16::try_from(span_list.len())
            .unwrap_or(u16::MAX)
            .to_le_bytes(),
    );
    out.extend_from_slice(
        &u16::try_from(tables.len())
            .unwrap_or(u16::MAX)
            .to_le_bytes(),
    );

    // Text & Spans.
    out.extend_from_slice(text.as_bytes());
    for s in span_list {
        out.extend_from_slice(&s.start_byte.to_le_bytes());
        out.extend_from_slice(&s.end_byte.to_le_bytes());
        out.extend_from_slice(&s.style_flags.to_le_bytes());
        // **The two padding bytes are written explicitly.** `TextIntervalSpan` is `#[repr(C)]` and
        // 16 bytes: `4 + 4 + 2 + 2 pad + 4`. Writing only its four fields is 14 bytes, which reads
        // back as `start end flags <2 bytes of the next span's start> color` -- silently, because
        // every length in the span list is implied rather than declared. The `debug_assert` at the
        // end of this function is what caught it; writing the pad by hand is what keeps the record
        // 16 bytes whatever a future change to the struct's field order would do.
        out.extend_from_slice(&[0u8; 2]);
        out.extend_from_slice(&s.color_rgb.to_le_bytes());
    }

    // Table & Math States.
    for t in tables {
        out.extend_from_slice(&t.rows.to_le_bytes());
        out.extend_from_slice(&t.cols.to_le_bytes());
        for w in t.col_widths {
            out.extend_from_slice(&w.to_le_bytes());
        }
        out.extend_from_slice(&t.start_byte.to_le_bytes());
        out.extend_from_slice(&t.end_byte.to_le_bytes());
    }
    let mut math = Vec::new();
    for_each_math_span(text.as_bytes(), |span| {
        math.push((span.start, span.end, span.closed));
    });
    out.extend_from_slice(&u32::try_from(math.len()).unwrap_or(u32::MAX).to_le_bytes());
    for (start, end, closed) in &math {
        out.extend_from_slice(&start.to_le_bytes());
        out.extend_from_slice(&end.to_le_bytes());
        out.push(u8::from(*closed));
        out.extend_from_slice(&[0u8; 3]);
    }

    // Asset Catalog. Last, so it reads to end of input.
    assets.encode_into(&mut out);

    debug_assert_eq!(
        out.len(),
        HEADER_LEN
            + text.len()
            + span_list.len() * SPAN_BYTES
            + tables.len() * TABLE_BYTES
            + 4
            + math.len() * MATH_BYTES
            + assets.encoded_len()
    );
    out
}

/// Read a payload back.
///
/// Every length is checked against what is actually present before it is used, and every count is
/// checked against the bytes that remain before anything is allocated. A truncated payload is refused
/// whole: a half-read document is worse than a refused one, because the failure would surface as
/// missing text three screens down rather than as an error at open.
pub fn decode(input: &[u8]) -> Result<Decoded, PayloadError> {
    if input.len() < HEADER_LEN {
        return Err(PayloadError::TooShort {
            have: input.len(),
            need: HEADER_LEN,
        });
    }
    if input[OFF_MAGIC..OFF_MAGIC + 4] != MAGIC {
        return Err(PayloadError::BadMagic);
    }
    let format = u16::from_le_bytes([input[OFF_FORMAT], input[OFF_FORMAT + 1]]);
    if format != FORMAT {
        return Err(PayloadError::UnsupportedFormat {
            found: format,
            want: FORMAT,
        });
    }
    let flags = u16::from_le_bytes([input[OFF_FLAGS], input[OFF_FLAGS + 1]]);
    if flags & !FLAG_MASK != 0 {
        return Err(PayloadError::UnknownFlags { found: flags });
    }
    let text_len = u32::from_le_bytes([
        input[OFF_TEXT_LEN],
        input[OFF_TEXT_LEN + 1],
        input[OFF_TEXT_LEN + 2],
        input[OFF_TEXT_LEN + 3],
    ]);
    let span_count =
        u16::from_le_bytes([input[OFF_SPAN_COUNT], input[OFF_SPAN_COUNT + 1]]) as usize;
    let table_count =
        u16::from_le_bytes([input[OFF_TABLE_COUNT], input[OFF_TABLE_COUNT + 1]]) as usize;

    let mut cursor = HEADER_LEN;
    let text_end = cursor
        .checked_add(text_len as usize)
        .ok_or(PayloadError::TextOverruns {
            declared: text_len,
            have: input.len(),
        })?;
    if text_end > input.len() {
        return Err(PayloadError::TextOverruns {
            declared: text_len,
            have: input.len(),
        });
    }
    let text_bytes = &input[cursor..text_end];
    cursor = text_end;
    // `from_utf8` rather than `from_utf8_lossy`: a lossy decode would put U+FFFD into the document at
    // the corrupt byte, and every byte offset the span map and the table map hold would then point
    // at the wrong character. Refusing is the only answer that keeps the offsets honest.
    let text = std::str::from_utf8(text_bytes)
        .map_err(|_| PayloadError::NotUtf8)?
        .to_owned();

    // Text & Spans.
    let span_bytes = span_count
        .checked_mul(SPAN_BYTES)
        .ok_or(PayloadError::RecordOverruns {
            what: "spans",
            need: usize::MAX,
            have: input.len(),
        })?;
    let span_end = cursor
        .checked_add(span_bytes)
        .ok_or(PayloadError::RecordOverruns {
            what: "spans",
            need: usize::MAX,
            have: input.len(),
        })?;
    if span_end > input.len() {
        return Err(PayloadError::RecordOverruns {
            what: "spans",
            need: span_end,
            have: input.len(),
        });
    }
    let mut span_list = Vec::with_capacity(span_count);
    for _ in 0..span_count {
        let s = &input[cursor..cursor + SPAN_BYTES];
        let start_byte = u32::from_le_bytes([s[0], s[1], s[2], s[3]]);
        let end_byte = u32::from_le_bytes([s[4], s[5], s[6], s[7]]);
        let style_flags = u16::from_le_bytes([s[8], s[9]]);
        let color_rgb = u32::from_le_bytes([s[12], s[13], s[14], s[15]]);
        span_list.push(TextIntervalSpan::styled(
            start_byte,
            end_byte,
            style_flags,
            color_rgb,
        ));
        cursor += SPAN_BYTES;
    }
    // `from_spans` rather than a `style_range` per span: the list has to come back *as it was
    // stored*, and `style_range` normalises, which merges adjacent identically-styled spans. It
    // validates the sorted / non-overlapping / gap-free invariants instead.
    let text_len_u32 = u32::try_from(text.len()).unwrap_or(u32::MAX);
    let spans = SpanMap::from_spans(span_list, text_len_u32)?;

    // Table & Math States.
    let table_bytes = table_count
        .checked_mul(TABLE_BYTES)
        .ok_or(PayloadError::RecordOverruns {
            what: "tables",
            need: usize::MAX,
            have: input.len(),
        })?;
    let table_end = cursor
        .checked_add(table_bytes)
        .ok_or(PayloadError::RecordOverruns {
            what: "tables",
            need: usize::MAX,
            have: input.len(),
        })?;
    if table_end > input.len() {
        return Err(PayloadError::RecordOverruns {
            what: "tables",
            need: table_end,
            have: input.len(),
        });
    }
    let mut tables = Vec::with_capacity(table_count);
    for _ in 0..table_count {
        let t = &input[cursor..cursor + TABLE_BYTES];
        let rows = u16::from_le_bytes([t[0], t[1]]);
        let cols = u16::from_le_bytes([t[2], t[3]]);
        let mut col_widths = [1u16; TableSpan::MAX_COLS];
        for (i, w) in col_widths.iter_mut().enumerate() {
            let at = 4 + 2 * i;
            *w = u16::from_le_bytes([t[at], t[at + 1]]);
        }
        let start_byte = u32::from_le_bytes([t[20], t[21], t[22], t[23]]);
        let end_byte = u32::from_le_bytes([t[24], t[25], t[26], t[27]]);
        tables.push(TableSpan {
            rows,
            cols,
            col_widths,
            start_byte,
            end_byte,
        });
        cursor += TABLE_BYTES;
    }

    let math_count = read_u32(input, &mut cursor)? as usize;
    // `math_start` is captured **before** the walk below, not after: the walk advances `cursor` past
    // the records, and the cross-check needs where the records began. Reading them from the post-walk
    // cursor compares the math state against the *catalog*, which fails for every document that has
    // a formula and passes for every document that has none -- a gate that is exactly backwards, and
    // one that a document with no formula would have kept green.
    let math_start = cursor;
    for i in 0..math_count {
        let at = math_start + i * MATH_BYTES;
        if at + MATH_BYTES > input.len() {
            return Err(PayloadError::RecordOverruns {
                what: "math spans",
                need: at + MATH_BYTES,
                have: input.len(),
            });
        }
    }
    let stored_math = math_count;
    cursor = math_start + stored_math * MATH_BYTES;

    // The cross-check. Math is derived from the text, so the stored spans are redundant -- and that
    // is exactly why they are worth storing: an off-by-one in this file's cursor arithmetic moves
    // the asset catalog to the wrong offset, and without this the failure would be a catalog that
    // decodes to *some* images. Comparing counts catches it here, where the error can still name the
    // cause.
    let derived_math = math_span_count(text_bytes);
    if stored_math != derived_math {
        return Err(PayloadError::MathMismatch {
            stored: stored_math,
            derived: derived_math,
        });
    }
    // The stored spans themselves are read back and compared, not just counted, so a payload holding
    // the right *number* of the wrong spans is still refused -- and named, so the report says which
    // formula moved rather than only that one did.
    let mut bad: Option<(u32, u32, bool)> = None;
    let mut derived = 0usize;
    for_each_math_span(text_bytes, |span: MathSpan| {
        if bad.is_some() {
            return;
        }
        let at = math_start + derived * MATH_BYTES;
        derived += 1;
        let Some(m) = input.get(at..at + MATH_BYTES) else {
            bad = Some((span.start, span.end, span.closed));
            return;
        };
        let start = u32::from_le_bytes([m[0], m[1], m[2], m[3]]);
        let end = u32::from_le_bytes([m[4], m[5], m[6], m[7]]);
        let closed = m[8] != 0;
        // The 3 padding bytes are checked to be zero, which is what makes the record's 12 bytes
        // deterministic: without it a payload could carry a byte here and the round trip would not be
        // the identity.
        let pad_zero = m[9..MATH_BYTES].iter().all(|&b| b == 0);
        if start != span.start || end != span.end || closed != span.closed || !pad_zero {
            bad = Some((span.start, span.end, span.closed));
        }
    });
    if let Some((start, end, closed)) = bad {
        return Err(PayloadError::MathStateMismatch { start, end, closed });
    }

    // Asset Catalog: the last section, so it is everything that is left.
    let assets = AssetCatalog::decode(&input[cursor..])?;
    // `flags` said whether a catalog was present. A payload with the flag clear and bytes after the
    // tables would be a payload this build cannot fully account for, and reading it as an empty
    // catalog would drop data silently -- so the flag is checked, not ignored.
    if (flags & FLAG_ASSETS != 0) != !assets.is_empty() {
        return Err(PayloadError::UnknownFlags { found: flags });
    }

    Ok(Decoded {
        text,
        spans,
        tables,
        assets,
    })
}

/// Read a `u32` little-endian at `*cursor` and advance it by 4.
///
/// The one place a count is read, so "every length in this format is little-endian" is a statement
/// about a single function rather than a promise repeated at eight call sites.
fn read_u32(input: &[u8], cursor: &mut usize) -> Result<u32, PayloadError> {
    let end = cursor.checked_add(4).ok_or(PayloadError::RecordOverruns {
        what: "a count",
        need: usize::MAX,
        have: input.len(),
    })?;
    if end > input.len() {
        return Err(PayloadError::RecordOverruns {
            what: "a count",
            need: end,
            have: input.len(),
        });
    }
    let v = u32::from_le_bytes([
        input[*cursor],
        input[*cursor + 1],
        input[*cursor + 2],
        input[*cursor + 3],
    ]);
    *cursor = end;
    Ok(v)
}

/// The byte offsets of every image anchor in `text`.
///
/// Exposed here rather than left to callers because "where are the images" is the same question for
/// every one of them -- the session's paint, the session's line heights, and both exporters -- and
/// three copies of a three-byte scan is three chances to disagree.
#[must_use]
pub fn image_offsets(text: &[u8]) -> Vec<u32> {
    scan_anchors(text)
}

/// Bytes the anchor character occupies, for callers that need to skip one.
pub const fn anchor_len() -> usize {
    ANCHOR_BYTES.len()
}
