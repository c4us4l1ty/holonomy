//! Streaming import, explicit plaintext-egress export, and one-way v5.1.0 -> v6 container
//! migration (`ROADMAP.md` Block 10).
//!
//! All three paths enforce bounded streaming residency: plaintext moves in `<= CAPACITY` (`16 KiB`)
//! chunks through `zeroize::Zeroizing` buffers and is wiped immediately as each segment commits or
//! leaves scope. Outbound plaintext export never runs silently — it returns an explicit
//! [`ExportReport`] disclosing plaintext egress outside the container.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, Write};
use std::num::NonZeroU32;
use std::path::Path;

use holonomy_line::{LineManifest, LineSlab};
use holonomy_seal::v5::{
    open_v5_streaming, V5Error, V5PadSource, V5Passphrase, V5_PAYLOAD_PLAINTEXT_PER_SLOT,
};
use holonomy_seal::{KdfParamsRecord, ParamsClass, PassphraseInput};
use holonomy_text::{
    DocStyle, DocStyleSpan, FontFamily, Heading, ListKind, SegmentId, SegmentSource, StyleMap,
    CAPACITY,
};
use zeroize::Zeroizing;

use crate::store::{to_line_manifest, Document, DocumentSource, StoreError};

/// Disclosure notice attached to every [`ExportReport`].
pub const PLAINTEXT_EGRESS_DISCLOSURE: &str =
    "PLAINTEXT EGRESS: exported file contains unencrypted UTF-8 outside the authenticated container";

const MAX_STREAM_LINE_BYTES: usize = 65_536;
const MAX_LINK_URL_BYTES: usize = 4_096;

/// Supported text interchange formats for [`import_from_reader`] and [`export_to_writer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferFormat {
    /// Raw lossless UTF-8 plain text (`.txt`).
    PlainText,
    /// CommonMark-subset Markdown (`.md`) mapped to/from `holonomy-text` [`DocStyle`] spans.
    Markdown,
}

impl TransferFormat {
    /// Infers [`TransferFormat::Markdown`] for `.md`/`.markdown` paths and
    /// [`TransferFormat::PlainText`] for everything else.
    #[must_use]
    pub fn from_path(path: &Path) -> Self {
        match path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("md" | "markdown") => Self::Markdown,
            _ => Self::PlainText,
        }
    }

    /// Short human label for CLI/UI reporting.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::PlainText => "plain-text",
            Self::Markdown => "markdown",
        }
    }
}

/// Result returned after importing plaintext into a new v6 `.wavefunction` document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportReport {
    /// Interchange format parsed during import.
    pub format: TransferFormat,
    /// Number of committed encrypted segments created in the destination container.
    pub segments_written: u64,
    /// Number of UTF-8 plaintext bytes stored inside the active document spine.
    pub plaintext_bytes: u64,
    /// Number of canonical style runs persisted in the manifest (`0` for plain text).
    pub style_runs: usize,
}

/// Explicit plaintext-egress report returned by [`export_to_writer`] / [`export_to_path`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportReport {
    /// Target path or label written to.
    pub destination: String,
    /// Interchange format emitted.
    pub format: TransferFormat,
    /// Number of segments decrypted while streaming the document spine.
    pub segments_read: u64,
    /// Total unencrypted UTF-8 bytes emitted outside the container.
    pub bytes_written: u64,
    /// Explicit warning string suitable for surfacing in CLI/GUI UI (`PLAINTEXT_EGRESS_DISCLOSURE`).
    pub disclosure: &'static str,
}

/// Result of migrating a Holonomy v5.1.0 container into a fresh v6 `.wavefunction`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V5MigrateReport {
    /// Stratum unlocked inside the v5.1.0 source (`1..=4`).
    pub source_stratum: u8,
    /// Monotonic pad cursor authenticated inside the v5.1.0 directory ledger.
    pub source_cursor: u64,
    /// Number of encrypted payload slots authenticated in the v5.1.0 container.
    pub source_slots: usize,
    /// Number of `<= CAPACITY` v6 segments committed in the destination container.
    pub imported_segments: u64,
    /// Number of validated UTF-8 bytes stored inside the destination v6 container.
    pub imported_bytes: u64,
    /// Segment count independently re-authenticated by `WorldLine::verify` after commit.
    pub verified_segments: u64,
}

/// Failure during streaming import, export, or v5 migration.
#[derive(Debug)]
pub enum TransferError {
    /// Store / cryptography refusal or persistence error.
    Store(StoreError),
    /// Legacy v5.1.0 reader or authentication failure.
    V5(V5Error),
    /// Filesystem I/O failure on the interchange source or destination.
    Io(io::Error),
    /// Source stream did not contain valid UTF-8.
    InvalidUtf8(String),
    /// Destination already exists and overwriting was not explicitly enabled.
    AlreadyExists(String),
    /// World-line commit failed mid-transfer.
    Line(String),
}

impl core::fmt::Display for TransferError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Store(err) => write!(f, "{err}"),
            Self::V5(err) => write!(f, "{err}"),
            Self::Io(err) => write!(f, "interchange I/O error: {err}"),
            Self::InvalidUtf8(reason) => write!(f, "invalid UTF-8 input: {reason}"),
            Self::AlreadyExists(path) => {
                write!(f, "refusing to overwrite existing destination {path}")
            }
            Self::Line(reason) => write!(f, "transfer failed: {reason}"),
        }
    }
}

impl std::error::Error for TransferError {}

impl From<StoreError> for TransferError {
    fn from(err: StoreError) -> Self {
        Self::Store(err)
    }
}

impl From<V5Error> for TransferError {
    fn from(err: V5Error) -> Self {
        Self::V5(err)
    }
}

impl From<io::Error> for TransferError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

fn utf8_prefix_limit(bytes: &[u8], max_bytes: usize) -> usize {
    let mut end = bytes.len().min(max_bytes);
    while end > 0 && std::str::from_utf8(&bytes[..end]).is_err() {
        end -= 1;
    }
    end
}

/// Imports a `.txt` or `.md` file into a brand-new encrypted v6 `.wavefunction` document using the
/// calibrated host Argon2id schedule.
pub fn import_from_path(
    source_path: &Path,
    dest_path: &Path,
    passphrase: &PassphraseInput,
    format: Option<TransferFormat>,
) -> Result<(Document, ImportReport), TransferError> {
    let calibration = holonomy_seal::calibrate();
    import_from_path_with(source_path, dest_path, passphrase, calibration.params, format)
}

/// Imports a `.txt` or `.md` file into a new encrypted v6 `.wavefunction` using `params`.
pub fn import_from_path_with(
    source_path: &Path,
    dest_path: &Path,
    passphrase: &PassphraseInput,
    params: KdfParamsRecord,
    format: Option<TransferFormat>,
) -> Result<(Document, ImportReport), TransferError> {
    let file = File::open(source_path)?;
    let fmt = format.unwrap_or_else(|| TransferFormat::from_path(source_path));
    import_from_reader(BufReader::new(file), dest_path, passphrase, params, fmt)
}

/// Streams UTF-8 plain text or Markdown from `reader` into a newly created v6 `.wavefunction` at
/// `dest_path`, maintaining bounded `O(CAPACITY)` plaintext memory and cleaning up partial files on
/// error.
pub fn import_from_reader<R: BufRead>(
    reader: R,
    dest_path: &Path,
    passphrase: &PassphraseInput,
    params: KdfParamsRecord,
    format: TransferFormat,
) -> Result<(Document, ImportReport), TransferError> {
    let doc = Document::create_with_input(dest_path, passphrase, params)?;
    let res = stream_import_into_doc(reader, &doc, format);
    match res {
        Ok(report) => Ok((doc, report)),
        Err(err) => {
            drop(doc);
            let _ = fs::remove_file(dest_path);
            Err(err)
        }
    }
}

fn stream_import_into_doc<R: BufRead>(
    mut reader: R,
    doc: &Document,
    format: TransferFormat,
) -> Result<ImportReport, TransferError> {
    match format {
        TransferFormat::PlainText => stream_import_plaintext(&mut reader, doc),
        TransferFormat::Markdown => stream_import_markdown(&mut reader, doc),
    }
}

fn stream_import_plaintext<R: BufRead>(
    reader: &mut R,
    doc: &Document,
) -> Result<ImportReport, TransferError> {
    let mut tx = doc
        .line
        .borrow_mut()
        .line
        .start_streaming_tx()
        .map_err(|e| TransferError::Line(e.to_string()))?;

    let mut read_buf = Zeroizing::new([0u8; CAPACITY]);
    let mut pending = Zeroizing::new(Vec::<u8>::with_capacity(CAPACITY + 4));
    let mut segments_written = 0u64;
    let mut plaintext_bytes = 0u64;

    let res = (|| -> Result<(), TransferError> {
        loop {
            let n = reader.read(&mut read_buf[..])?;
            if n == 0 {
                break;
            }
            pending.extend_from_slice(&read_buf[..n]);
            while pending.len() >= CAPACITY {
                let cut = match std::str::from_utf8(&pending[..CAPACITY]) {
                    Ok(_) => CAPACITY,
                    Err(e) if e.error_len().is_none() => e.valid_up_to(),
                    Err(e) => {
                        return Err(TransferError::InvalidUtf8(format!(
                            "byte offset {}",
                            plaintext_bytes + e.valid_up_to() as u64
                        )));
                    }
                };
                if cut == 0 {
                    return Err(TransferError::InvalidUtf8(
                        "invalid multi-byte UTF-8 sequence".to_owned(),
                    ));
                }
                tx.push_segment(&pending[..cut])
                    .map_err(|e| TransferError::Line(e.to_string()))?;
                segments_written += 1;
                plaintext_bytes += cut as u64;
                pending.drain(..cut);
            }
        }
        if !pending.is_empty() {
            if let Err(e) = std::str::from_utf8(&pending) {
                return Err(TransferError::InvalidUtf8(format!(
                    "trailing byte offset {}: {e}",
                    plaintext_bytes + e.valid_up_to() as u64
                )));
            }
            while !pending.is_empty() {
                let cut = utf8_prefix_limit(&pending, CAPACITY);
                if cut == 0 {
                    return Err(TransferError::InvalidUtf8(
                        "trailing non-UTF-8 byte sequence".to_owned(),
                    ));
                }
                tx.push_segment(&pending[..cut])
                    .map_err(|e| TransferError::Line(e.to_string()))?;
                segments_written += 1;
                plaintext_bytes += cut as u64;
                pending.drain(..cut);
            }
        }
        Ok(())
    })();

    if let Err(err) = res {
        let _ = doc.line.borrow_mut().line.abort_streaming_tx(tx);
        return Err(err);
    }

    doc.line
        .borrow_mut()
        .line
        .finish_streaming_tx(tx, None)
        .map_err(|e| TransferError::Line(e.to_string()))?;

    Ok(ImportReport {
        format: TransferFormat::PlainText,
        segments_written,
        plaintext_bytes,
        style_runs: 0,
    })
}

struct StreamChunkAccumulator {
    pending: Zeroizing<Vec<u8>>,
    slabs: Vec<LineSlab>,
    styles: Vec<DocStyleSpan>,
    links: Vec<String>,
    link_index: HashMap<String, NonZeroU32>,
    doc_offset: u64,
    segments_written: u64,
}

impl StreamChunkAccumulator {
    fn new() -> Self {
        Self {
            pending: Zeroizing::new(Vec::with_capacity(CAPACITY + 4)),
            slabs: Vec::new(),
            styles: Vec::new(),
            links: Vec::new(),
            link_index: HashMap::new(),
            doc_offset: 0,
            segments_written: 0,
        }
    }

    fn intern_link(&mut self, url: &str) -> Option<NonZeroU32> {
        if url.is_empty() || url.len() > MAX_LINK_URL_BYTES {
            return None;
        }
        if let Some(&id) = self.link_index.get(url) {
            return Some(id);
        }
        let next = u32::try_from(self.links.len() + 1)
            .ok()
            .and_then(NonZeroU32::new)?;
        self.links.push(url.to_owned());
        self.link_index.insert(url.to_owned(), next);
        Some(next)
    }

    fn push_styled(
        &mut self,
        text: &str,
        style: DocStyle,
        tx: &mut holonomy_line::StreamingTx,
    ) -> Result<(), TransferError> {
        if text.is_empty() {
            return Ok(());
        }
        let start = self.doc_offset;
        let end = start
            .checked_add(text.len() as u64)
            .ok_or_else(|| TransferError::Line("document length overflow".to_owned()))?;
        if !style.is_default() {
            if let Some(prev) = self.styles.last_mut() {
                if prev.end == start && prev.style == style {
                    prev.end = end;
                } else {
                    self.styles.push(DocStyleSpan { start, end, style });
                }
            } else {
                self.styles.push(DocStyleSpan { start, end, style });
            }
        }
        self.pending.extend_from_slice(text.as_bytes());
        self.doc_offset = end;
        self.flush_full(tx)
    }

    fn flush_full(&mut self, tx: &mut holonomy_line::StreamingTx) -> Result<(), TransferError> {
        while self.pending.len() >= CAPACITY {
            let cut = utf8_prefix_limit(&self.pending, CAPACITY);
            if cut == 0 {
                return Err(TransferError::InvalidUtf8(
                    "invalid UTF-8 boundary during stream flush".to_owned(),
                ));
            }
            let slab = tx
                .push_segment(&self.pending[..cut])
                .map_err(|e| TransferError::Line(e.to_string()))?;
            self.slabs.push(slab);
            self.segments_written += 1;
            self.pending.drain(..cut);
        }
        Ok(())
    }

    fn finish_tail(&mut self, tx: &mut holonomy_line::StreamingTx) -> Result<(), TransferError> {
        while !self.pending.is_empty() {
            let cut = utf8_prefix_limit(&self.pending, CAPACITY);
            if cut == 0 {
                return Err(TransferError::InvalidUtf8(
                    "invalid UTF-8 boundary at stream tail".to_owned(),
                ));
            }
            let slab = tx
                .push_segment(&self.pending[..cut])
                .map_err(|e| TransferError::Line(e.to_string()))?;
            self.slabs.push(slab);
            self.segments_written += 1;
            self.pending.drain(..cut);
        }
        Ok(())
    }
}

fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    line_buf: &mut Zeroizing<Vec<u8>>,
) -> Result<bool, TransferError> {
    line_buf.clear();
    loop {
        let avail = reader.fill_buf()?;
        if avail.is_empty() {
            return Ok(!line_buf.is_empty());
        }
        if let Some(pos) = avail.iter().position(|&b| b == b'\n') {
            let take = pos + 1;
            line_buf.extend_from_slice(&avail[..take]);
            reader.consume(take);
            return Ok(true);
        }
        let room = MAX_STREAM_LINE_BYTES.saturating_sub(line_buf.len());
        if room == 0 {
            return Ok(true);
        }
        let take = avail.len().min(room);
        // Avoid splitting a multi-byte UTF-8 sequence when clipping an extra-long line.
        let safe_take = if take == avail.len() {
            take
        } else {
            let mut safe = take;
            while safe > 0 && (avail[safe] & 0b1100_0000) == 0b1000_0000 {
                safe -= 1;
            }
            if safe == 0 {
                take
            } else {
                safe
            }
        };
        line_buf.extend_from_slice(&avail[..safe_take]);
        reader.consume(safe_take);
        if line_buf.len() >= MAX_STREAM_LINE_BYTES {
            return Ok(true);
        }
    }
}

fn parse_ordered_list_prefix(s: &str) -> Option<(u32, usize)> {
    let bytes = s.as_bytes();
    let mut digits = 0usize;
    while digits < bytes.len() && digits < 9 && bytes[digits].is_ascii_digit() {
        digits += 1;
    }
    if digits == 0 || digits + 1 >= bytes.len() {
        return None;
    }
    if bytes[digits] != b'.' || bytes[digits + 1] != b' ' {
        return None;
    }
    let start: u32 = s[..digits].parse().ok()?;
    Some((start.max(1), digits + 2))
}

fn is_horizontal_rule(trimmed: &str) -> bool {
    if trimmed.len() < 3 {
        return false;
    }
    let mut marker = None;
    let mut count = 0usize;
    for ch in trimmed.chars() {
        if ch == ' ' {
            continue;
        }
        if ch != '-' && ch != '*' && ch != '_' {
            return false;
        }
        match marker {
            None => marker = Some(ch),
            Some(m) if m != ch => return false,
            _ => {}
        }
        count += 1;
    }
    count >= 3
}

fn stream_markdown_inline(
    input: &str,
    base_style: DocStyle,
    acc: &mut StreamChunkAccumulator,
    tx: &mut holonomy_line::StreamingTx,
) -> Result<(), TransferError> {
    let mut plain_run = String::new();
    let mut i = 0usize;
    while i < input.len() {
        let rest = &input[i..];
        if rest.starts_with('\\') && rest.len() > 1 {
            if let Some(escaped) = rest[1..].chars().next() {
                plain_run.push(escaped);
                i += 1 + escaped.len_utf8();
                continue;
            }
        }
        if rest.starts_with('`') {
            if let Some(end) = rest[1..].find('`') {
                if !plain_run.is_empty() {
                    acc.push_styled(&plain_run, base_style, tx)?;
                    plain_run.clear();
                }
                let inner = &rest[1..1 + end];
                let mut code_style = base_style;
                code_style.code_span = true;
                code_style.family = FontFamily::Mono;
                if code_style.size_milli == 1_000 {
                    code_style.size_milli = 900;
                }
                acc.push_styled(inner, code_style, tx)?;
                i += 1 + end + 1;
                continue;
            }
        }
        if rest.starts_with("**") {
            if let Some(end) = rest[2..].find("**") {
                if end > 0 {
                    if !plain_run.is_empty() {
                        acc.push_styled(&plain_run, base_style, tx)?;
                        plain_run.clear();
                    }
                    let mut bold_style = base_style;
                    bold_style.bold = true;
                    stream_markdown_inline(&rest[2..2 + end], bold_style, acc, tx)?;
                    i += 2 + end + 2;
                    continue;
                }
            }
        }
        if rest.starts_with("~~") {
            if let Some(end) = rest[2..].find("~~") {
                if end > 0 {
                    if !plain_run.is_empty() {
                        acc.push_styled(&plain_run, base_style, tx)?;
                        plain_run.clear();
                    }
                    let mut strike_style = base_style;
                    strike_style.strikethrough = true;
                    stream_markdown_inline(&rest[2..2 + end], strike_style, acc, tx)?;
                    i += 2 + end + 2;
                    continue;
                }
            }
        }
        if let Some(delim) = rest.chars().next().filter(|&c| c == '*' || c == '_') {
            let prev_is_alnum = i > 0
                && input[..i]
                    .chars()
                    .next_back()
                    .is_some_and(char::is_alphanumeric);
            if !(delim == '_' && prev_is_alnum) {
                if let Some(end) = rest[1..].find(delim) {
                    let inner = &rest[1..1 + end];
                    let after_is_alnum = rest[1 + end + 1..]
                        .chars()
                        .next()
                        .is_some_and(char::is_alphanumeric);
                    if !inner.is_empty()
                        && !inner.starts_with(' ')
                        && !inner.ends_with(' ')
                        && !(delim == '_' && after_is_alnum)
                    {
                        if !plain_run.is_empty() {
                            acc.push_styled(&plain_run, base_style, tx)?;
                            plain_run.clear();
                        }
                        let mut italic_style = base_style;
                        italic_style.italic = true;
                        stream_markdown_inline(inner, italic_style, acc, tx)?;
                        i += 1 + end + 1;
                        continue;
                    }
                }
            }
        }
        if rest.starts_with('[') {
            if let Some(close_bracket) = rest[1..].find("](") {
                let label = &rest[1..1 + close_bracket];
                let after = &rest[1 + close_bracket + 2..];
                if let Some(close_paren) = after.find(')') {
                    let url = &after[..close_paren];
                    if !label.is_empty() && !url.is_empty() && !url.contains(char::is_whitespace) {
                        if !plain_run.is_empty() {
                            acc.push_styled(&plain_run, base_style, tx)?;
                            plain_run.clear();
                        }
                        let link_id = acc.intern_link(url);
                        let mut link_style = base_style;
                        link_style.link_id = link_id;
                        if link_id.is_some() {
                            link_style.underline = true;
                        }
                        stream_markdown_inline(label, link_style, acc, tx)?;
                        i += 1 + close_bracket + 2 + close_paren + 1;
                        continue;
                    }
                }
            }
        }
        if let Some(ch) = rest.chars().next() {
            plain_run.push(ch);
            i += ch.len_utf8();
        } else {
            break;
        }
    }
    if !plain_run.is_empty() {
        acc.push_styled(&plain_run, base_style, tx)?;
    }
    Ok(())
}

fn stream_import_markdown<R: BufRead>(
    reader: &mut R,
    doc: &Document,
) -> Result<ImportReport, TransferError> {
    let mut tx = doc
        .line
        .borrow_mut()
        .line
        .start_streaming_tx()
        .map_err(|e| TransferError::Line(e.to_string()))?;

    let mut line_buf = Zeroizing::new(Vec::<u8>::with_capacity(4_096));
    let mut acc = StreamChunkAccumulator::new();

    let res = (|| -> Result<(), TransferError> {
        while read_bounded_line(reader, &mut line_buf)? {
            let line_str = std::str::from_utf8(&line_buf).map_err(|e| {
                TransferError::InvalidUtf8(format!(
                    "byte offset {}: {e}",
                    acc.doc_offset + e.valid_up_to() as u64
                ))
            })?;
            let (body_raw, has_newline) = match line_str.strip_suffix('\n') {
                Some(stripped) => (stripped.strip_suffix('\r').unwrap_or(stripped), true),
                None => (line_str, false),
            };

            let mut rest = body_raw;
            let mut style = DocStyle::default();

            let mut bq = 0u8;
            while let Some(after) = rest.strip_prefix("> ").or_else(|| {
                if rest == ">" {
                    Some("")
                } else {
                    None
                }
            }) {
                bq = bq.saturating_add(1).min(8);
                rest = after;
            }
            style.blockquote = bq;

            if is_horizontal_rule(rest.trim()) {
                style.horizontal_rule = true;
                acc.push_styled("---", style, &mut tx)?;
                if has_newline {
                    acc.push_styled("\n", style, &mut tx)?;
                }
                continue;
            }

            if let Some(h) = rest.strip_prefix("### ") {
                style.heading = Heading::H3;
                style.size_milli = Heading::H3.default_size_milli();
                style.bold = Heading::H3.is_bold_by_default();
                rest = h;
            } else if let Some(h) = rest.strip_prefix("## ") {
                style.heading = Heading::H2;
                style.size_milli = Heading::H2.default_size_milli();
                style.bold = Heading::H2.is_bold_by_default();
                rest = h;
            } else if let Some(h) = rest.strip_prefix("# ") {
                style.heading = Heading::H1;
                style.size_milli = Heading::H1.default_size_milli();
                style.bold = Heading::H1.is_bold_by_default();
                rest = h;
            } else if let Some(item) = rest.strip_prefix("- ").or_else(|| rest.strip_prefix("* ")) {
                style.list = ListKind::Bullet { depth: 1 };
                rest = item;
            } else if let Some((start, consume)) = parse_ordered_list_prefix(rest) {
                style.list = ListKind::Numbered { depth: 1, start };
                rest = &rest[consume..];
            }

            stream_markdown_inline(rest, style, &mut acc, &mut tx)?;
            if has_newline {
                acc.push_styled("\n", DocStyle::default(), &mut tx)?;
            }
        }
        acc.finish_tail(&mut tx)?;
        Ok(())
    })();

    if let Err(err) = res {
        let _ = doc.line.borrow_mut().line.abort_streaming_tx(tx);
        return Err(err);
    }

    let style_map = StyleMap::from_raw(acc.styles, acc.links);
    let style_runs = style_map.runs().len();
    let explicit_manifest: Option<LineManifest> = if style_map.is_empty() {
        None
    } else {
        Some(to_line_manifest(acc.slabs, &style_map))
    };

    doc.line
        .borrow_mut()
        .line
        .finish_streaming_tx(tx, explicit_manifest.as_ref())
        .map_err(|e| TransferError::Line(e.to_string()))?;

    Ok(ImportReport {
        format: TransferFormat::Markdown,
        segments_written: acc.segments_written,
        plaintext_bytes: acc.doc_offset,
        style_runs,
    })
}

/// Explicitly exports an unlocked document spine to a filesystem path as unencrypted `.txt` or
/// `.md`. Refuses to overwrite an existing destination unless `overwrite` is `true`.
pub fn export_to_path(
    doc: &Document,
    dest_path: &Path,
    format: Option<TransferFormat>,
    overwrite: bool,
) -> Result<ExportReport, TransferError> {
    let mut opts = OpenOptions::new();
    opts.write(true);
    if overwrite {
        opts.create(true).truncate(true);
    } else {
        opts.create_new(true);
    }
    let file = match opts.open(dest_path) {
        Ok(f) => f,
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            return Err(TransferError::AlreadyExists(dest_path.display().to_string()));
        }
        Err(err) => return Err(TransferError::Io(err)),
    };
    let fmt = format.unwrap_or_else(|| TransferFormat::from_path(dest_path));
    export_to_writer(doc, file, &dest_path.display().to_string(), fmt)
}

/// Explicitly exports an unlocked document spine to `writer` in bounded `O(CAPACITY)` streaming
/// passes and returns [`ExportReport`].
pub fn export_to_writer<W: Write>(
    doc: &Document,
    mut writer: W,
    destination: &str,
    format: TransferFormat,
) -> Result<ExportReport, TransferError> {
    let spine = doc.spine()?;
    let mut source = doc.source();
    let mut segments_read = 0u64;
    let mut bytes_written = 0u64;

    match format {
        TransferFormat::PlainText => {
            let mut cached_id: Option<SegmentId> = None;
            let mut cached_buf: Zeroizing<Vec<u8>> = Zeroizing::new(Vec::new());
            for span in spine.spans_in(0, spine.len()) {
                let slab = span.slab();
                if cached_id != Some(slab.segment()) {
                    let loaded = source
                        .load(slab.segment())
                        .map_err(TransferError::Line)?;
                    *cached_buf = loaded;
                    cached_id = Some(slab.segment());
                    segments_read += 1;
                }
                let slice = &cached_buf[slab.start() as usize..slab.end() as usize];
                writer.write_all(slice)?;
                bytes_written += slice.len() as u64;
            }
        }
        TransferFormat::Markdown => {
            bytes_written = export_markdown_streaming(&spine, &mut source, &mut writer, &mut segments_read)?;
        }
    }
    writer.flush()?;

    Ok(ExportReport {
        destination: destination.to_owned(),
        format,
        segments_read,
        bytes_written,
        disclosure: PLAINTEXT_EGRESS_DISCLOSURE,
    })
}

fn export_markdown_streaming<W: Write>(
    spine: &holonomy_text::Spine,
    source: &mut DocumentSource,
    writer: &mut W,
    segments_read: &mut u64,
) -> Result<u64, TransferError> {
    let styles = spine.styles();
    let total_len = spine.len();
    if total_len == 0 {
        return Ok(0);
    }
    let mut bytes_written = 0u64;
    let mut line_start = 0u64;
    let mut cached_id: Option<SegmentId> = None;
    let mut cached_buf: Zeroizing<Vec<u8>> = Zeroizing::new(Vec::new());

    let mut pos = 0u64;
    let mut line_bytes = Zeroizing::new(Vec::<u8>::with_capacity(1_024));

    let flush_line = |line_start: u64,
                      line_buf: &[u8],
                      has_nl: bool,
                      writer: &mut W,
                      written: &mut u64|
     -> Result<(), TransferError> {
        let text = std::str::from_utf8(line_buf)
            .map_err(|e| TransferError::InvalidUtf8(e.to_string()))?;
        let line_end = line_start + line_buf.len() as u64;
        let head_style = if line_start < line_end {
            styles.style_at(line_start)
        } else {
            DocStyle::default()
        };

        let mut prefix = String::new();
        for _ in 0..head_style.blockquote {
            prefix.push_str("> ");
        }
        if head_style.horizontal_rule {
            if !prefix.is_empty() {
                writer.write_all(prefix.as_bytes())?;
                *written += prefix.len() as u64;
            }
            let out = if text.trim().is_empty() { "---" } else { text };
            writer.write_all(out.as_bytes())?;
            *written += out.len() as u64;
            if has_nl {
                writer.write_all(b"\n")?;
                *written += 1;
            }
            return Ok(());
        }
        match head_style.heading {
            Heading::None => {}
            Heading::Title | Heading::H1 => prefix.push_str("# "),
            Heading::H2 => prefix.push_str("## "),
            Heading::H3 => prefix.push_str("### "),
        }
        match head_style.list {
            ListKind::None => {}
            ListKind::Bullet { .. } => prefix.push_str("- "),
            ListKind::Numbered { start, .. } => {
                prefix.push_str(&format!("{start}. "));
            }
        }
        if !prefix.is_empty() {
            writer.write_all(prefix.as_bytes())?;
            *written += prefix.len() as u64;
        }

        if line_buf.is_empty() {
            if has_nl {
                writer.write_all(b"\n")?;
                *written += 1;
            }
            return Ok(());
        }

        let mut cursor = line_start;
        for span in styles.spans_in(line_start, line_end) {
            let rel_start = (span.start - line_start) as usize;
            let rel_end = (span.end - line_start) as usize;
            if span.start > cursor {
                let gap = text
                    .get((cursor - line_start) as usize..rel_start)
                    .ok_or_else(|| {
                        TransferError::InvalidUtf8("style span crosses UTF-8 boundary".to_owned())
                    })?;
                writer.write_all(gap.as_bytes())?;
                *written += gap.len() as u64;
            }
            let seg = text.get(rel_start..rel_end).ok_or_else(|| {
                TransferError::InvalidUtf8("style span crosses UTF-8 boundary".to_owned())
            })?;
            let st = span.style;
            let implicit_bold = st.heading.is_bold_by_default();
            let wrap_bold = st.bold && !implicit_bold;
            let link_url = st.link_id.and_then(|id| styles.link_target(id));

            if st.code_span {
                writer.write_all(b"`")?;
                *written += 1;
            } else {
                if wrap_bold {
                    writer.write_all(b"**")?;
                    *written += 2;
                }
                if st.italic {
                    writer.write_all(b"*")?;
                    *written += 1;
                }
                if st.strikethrough {
                    writer.write_all(b"~~")?;
                    *written += 2;
                }
                if link_url.is_some() {
                    writer.write_all(b"[")?;
                    *written += 1;
                }
            }
            writer.write_all(seg.as_bytes())?;
            *written += seg.len() as u64;
            if st.code_span {
                writer.write_all(b"`")?;
                *written += 1;
            } else {
                if let Some(url) = link_url {
                    let tail = format!("]({url})");
                    writer.write_all(tail.as_bytes())?;
                    *written += tail.len() as u64;
                }
                if st.strikethrough {
                    writer.write_all(b"~~")?;
                    *written += 2;
                }
                if st.italic {
                    writer.write_all(b"*")?;
                    *written += 1;
                }
                if wrap_bold {
                    writer.write_all(b"**")?;
                    *written += 2;
                }
            }
            cursor = span.end;
        }
        if cursor < line_end {
            let tail = &text[(cursor - line_start) as usize..];
            writer.write_all(tail.as_bytes())?;
            *written += tail.len() as u64;
        }
        if has_nl {
            writer.write_all(b"\n")?;
            *written += 1;
        }
        Ok(())
    };

    for span in spine.spans_in(0, total_len) {
        let slab = span.slab();
        if cached_id != Some(slab.segment()) {
            let loaded = source
                .load(slab.segment())
                .map_err(TransferError::Line)?;
            *cached_buf = loaded;
            cached_id = Some(slab.segment());
            *segments_read += 1;
        }
        let slice = &cached_buf[slab.start() as usize..slab.end() as usize];
        let mut idx = 0usize;
        while idx < slice.len() {
            if let Some(rel_nl) = slice[idx..].iter().position(|&b| b == b'\n') {
                line_bytes.extend_from_slice(&slice[idx..idx + rel_nl]);
                flush_line(line_start, &line_bytes, true, &mut writer, &mut bytes_written)?;
                pos += (rel_nl + 1) as u64;
                line_start = pos;
                line_bytes.clear();
                idx += rel_nl + 1;
            } else {
                line_bytes.extend_from_slice(&slice[idx..]);
                pos += (slice.len() - idx) as u64;
                idx = slice.len();
                if line_bytes.len() >= MAX_STREAM_LINE_BYTES {
                    let cut = utf8_prefix_limit(&line_bytes, MAX_STREAM_LINE_BYTES);
                    if cut > 0 {
                        flush_line(
                            line_start,
                            &line_bytes[..cut],
                            false,
                            &mut writer,
                            &mut bytes_written,
                        )?;
                        line_start += cut as u64;
                        line_bytes.drain(..cut);
                    }
                }
            }
        }
    }
    if !line_bytes.is_empty() {
        flush_line(line_start, &line_bytes, false, &mut writer, &mut bytes_written)?;
    }
    Ok(bytes_written)
}

/// Migrates an encrypted v5.1.0 container into a fresh, non-overwriting v6 `.wavefunction`
/// container using calibrated production Argon2id parameters.
///
/// The v5.1.0 source is opened read-only and never modified; every 64 KiB slot is decrypted,
/// checked for UTF-8 continuity across slot boundaries, re-sealed in `<= CAPACITY` (`16 KiB`)
/// v6 segments, verified via [`Document::verify`], and wiped on drop.
pub fn migrate_v5_container(
    v5_path: &Path,
    dest_v6_path: &Path,
    v5_passphrase: &V5Passphrase,
    v6_passphrase: &PassphraseInput,
    kappa_usb: &[u8; 32],
    pad: &V5PadSource,
    expected_cursor: Option<u64>,
) -> Result<V5MigrateReport, TransferError> {
    let calibration = holonomy_seal::calibrate();
    migrate_v5_container_with(
        v5_path,
        dest_v6_path,
        v5_passphrase,
        v6_passphrase,
        kappa_usb,
        pad,
        expected_cursor,
        ParamsClass::from_costs(calibration.params.costs()),
        calibration.params,
    )
}

/// Parameterized variant of [`migrate_v5_container`] allowing tests to run under `ParamsClass::Test`
/// while release builds continue enforcing the production KDF fence.
#[allow(clippy::too_many_arguments)]
pub fn migrate_v5_container_with(
    v5_path: &Path,
    dest_v6_path: &Path,
    v5_passphrase: &V5Passphrase,
    v6_passphrase: &PassphraseInput,
    kappa_usb: &[u8; 32],
    pad: &V5PadSource,
    expected_cursor: Option<u64>,
    v5_params: ParamsClass,
    v6_params: KdfParamsRecord,
) -> Result<V5MigrateReport, TransferError> {
    let mut source = File::open(v5_path)?;
    migrate_v5_reader_with(
        &mut source,
        dest_v6_path,
        v5_passphrase,
        v6_passphrase,
        kappa_usb,
        pad,
        expected_cursor,
        v5_params,
        v6_params,
    )
}

/// Reader-generic v5.1.0 -> v6 streaming migration engine.
#[allow(clippy::too_many_arguments)]
pub fn migrate_v5_reader_with<R: Read + Seek>(
    source: &mut R,
    dest_v6_path: &Path,
    v5_passphrase: &V5Passphrase,
    v6_passphrase: &PassphraseInput,
    kappa_usb: &[u8; 32],
    pad: &V5PadSource,
    expected_cursor: Option<u64>,
    v5_params: ParamsClass,
    v6_params: KdfParamsRecord,
) -> Result<V5MigrateReport, TransferError> {
    let doc = Document::create_with_input(dest_v6_path, v6_passphrase, v6_params)?;

    let res = (|| -> Result<V5MigrateReport, TransferError> {
        let mut tx = doc
            .line
            .borrow_mut()
            .line
            .start_streaming_tx()
            .map_err(|e| TransferError::Line(e.to_string()))?;

        let mut staging = Zeroizing::new(Vec::<u8>::with_capacity(
            V5_PAYLOAD_PLAINTEXT_PER_SLOT + 4,
        ));
        let mut imported_segments = 0u64;
        let mut imported_bytes = 0u64;

        let open_res = open_v5_streaming(
            source,
            v5_passphrase,
            kappa_usb,
            pad,
            expected_cursor,
            v5_params,
            |slot_idx, slot_bytes| {
                staging.extend_from_slice(slot_bytes);
                let valid_end = match std::str::from_utf8(&staging) {
                    Ok(_) => staging.len(),
                    Err(e) if e.error_len().is_none() => e.valid_up_to(),
                    Err(e) => {
                        return Err(format!(
                            "v5 slot {slot_idx} contains non-UTF-8 byte at offset {}: {e}",
                            e.valid_up_to()
                        ));
                    }
                };
                let mut offset = 0usize;
                while offset < valid_end {
                    let max_take = CAPACITY.min(valid_end - offset);
                    let cut = utf8_prefix_limit(&staging[offset..offset + max_take], max_take);
                    if cut == 0 {
                        return Err("invalid UTF-8 boundary while migrating v5 slot".to_owned());
                    }
                    tx.push_segment(&staging[offset..offset + cut])
                        .map_err(|e| e.to_string())?;
                    imported_segments += 1;
                    imported_bytes += cut as u64;
                    offset += cut;
                }
                staging.drain(..valid_end);
                Ok(())
            },
        );

        let v5_report = match open_res {
            Ok(rep) => rep,
            Err(err) => {
                let _ = doc.line.borrow_mut().line.abort_streaming_tx(tx);
                return Err(TransferError::V5(err));
            }
        };

        if !staging.is_empty() {
            let _ = doc.line.borrow_mut().line.abort_streaming_tx(tx);
            return Err(TransferError::InvalidUtf8(
                "incomplete trailing UTF-8 sequence at end of v5 container".to_owned(),
            ));
        }

        doc.line
            .borrow_mut()
            .line
            .finish_streaming_tx(tx, None)
            .map_err(|e| TransferError::Line(e.to_string()))?;

        let verify = doc.verify()?;
        Ok(V5MigrateReport {
            source_stratum: v5_report.stratum,
            source_cursor: v5_report.cursor,
            source_slots: v5_report.slot_count,
            imported_segments,
            imported_bytes,
            verified_segments: verify.segments_verified,
        })
    })();

    if res.is_err() {
        drop(doc);
        let _ = fs::remove_file(dest_v6_path);
    }
    res
}
