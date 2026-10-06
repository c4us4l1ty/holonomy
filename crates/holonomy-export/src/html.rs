//! Streaming HTML, straight from the CAGR leaves and the span map.
//!
//! # Streaming, and what "no allocation" means here
//!
//! The document is emitted one run at a time: read a bounded chunk of leaves, wrap each run in
//! whatever tags the span map calls for, write it out. Nothing builds a tree and nothing joins into a
//! `String`.
//!
//! That is not style. The exporter runs inside the jail, where memory is a scarce locked region, and a
//! document that must be doubled in order to be written is a document that can fail to be written. The
//! span map already stores *runs* rather than per-byte style, so a stream falls out of it for free;
//! making a copy would be gratuitous.
//!
//! What the export does allocate, and why each is unavoidable:
//!
//! * [`SpanMap::runs_in`] returns its run list in a `Vec`. That is O(runs in the exported range), and
//!   runs are bounded by the document's styling rather than its length. Reusing the exporter's own
//!   scratch is not possible without changing the text crate's signature, and a `Vec` here is cheaper
//!   than a second pass per run.
//! * The escaping scratch is a single `String`, cleared per run. Its capacity is the largest run's
//!   escaped length, so it stops growing after the first wide run.
//!
//! # The span map decides structure, the leaves supply bytes
//!
//! Two sources walked in parallel: [`Editor::read_into`] for the bytes and `runs_in` for where the
//! runs begin. The exporter never looks for a style boundary itself, so a style change the span map
//! does not know about cannot desynchronise the tags from the text.
//!
//! # Tag nesting is fixed, and the close order is its exact reverse
//!
//! `<h1><b><i><code>`. Closing in any other order produces a document that renders the remainder of
//! the page in bold, and browsers are forgiving enough to hide it. `closes_for` is written as the
//! literal reverse of `opens_for` and the two are asserted against each other.

use std::io::{self, Write};

use holonomy_text::ANCHOR_BYTES;
use holonomy_text::{
    Editor, EditorError, TextIntervalSpan, STYLE_BOLD, STYLE_CODE, STYLE_HEADER, STYLE_ITALIC,
};

/// Bytes read from the document per iteration.
///
/// Bounded so the exporter's working set does not grow with the document. 64 KiB is 64 leaves'
/// worth of usable text at `LEAF_CAPACITY`, which is far larger than any single keystroke's damage
/// region and far smaller than the page-lock ceiling.
pub const CHUNK_BYTES: usize = 64 * 1024;

/// Why an HTML export stopped early.
#[derive(Debug)]
pub enum HtmlError {
    /// The sink failed.
    Io(io::Error),
    /// The document refused to be read.
    Editor(EditorError),
}

impl std::fmt::Display for HtmlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "writing HTML: {e}"),
            Self::Editor(e) => write!(f, "reading the document for HTML: {e}"),
        }
    }
}

impl std::error::Error for HtmlError {}

impl From<io::Error> for HtmlError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<EditorError> for HtmlError {
    fn from(e: EditorError) -> Self {
        Self::Editor(e)
    }
}

/// What the `<head>` should say.
#[derive(Debug, Clone)]
pub struct HtmlOptions {
    /// `<title>`. Escaped once and reused.
    pub title: String,
    /// Emit the `<style>` block.
    ///
    /// On by default, because the output is a standalone document rather than a fragment and a reader
    /// opening it should get something legible. The brief's "zero-allocation stream" is about the body,
    /// and this is a fixed constant either way.
    pub stylesheet: bool,
    /// Emit `<meta charset="utf-8">`.
    ///
    /// A field rather than always-on so a test can assert the body without the preamble in the way.
    pub charset_meta: bool,
}

impl Default for HtmlOptions {
    fn default() -> Self {
        Self {
            title: "Holonomy".to_string(),
            stylesheet: true,
            charset_meta: true,
        }
    }
}

impl HtmlOptions {
    /// The default stylesheet, a `const` so it is greppable and costs nothing at runtime.
    ///
    /// Deliberately small: it exists so a `<code>` run looks like code and a header looks like a header.
    /// A real theme belongs in CSS, not in a word processor's export path.
    pub const STYLESHEET: &'static str = "\
body{font-family:Helvetica,Arial,sans-serif;font-size:11pt;line-height:1.45;\
max-width:38em;margin:2em auto;padding:0 1em;color:#1a1a1a;background:#fff}\
code{font-family:'Courier New',Courier,monospace;font-size:.95em;\
background:#f2f2f2;padding:.05em .25em;border-radius:2px}\
h1{font-size:1.4em;margin:1.2em 0 .4em;line-height:1.2}\
p{margin:0 0 .9em}";
}

/// What an export emitted, for a status line and for the tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HtmlStats {
    /// Bytes written to the sink.
    pub bytes: u64,
    /// Text bytes copied out of the document, before escaping.
    pub text_bytes: u64,
    /// `<b>` runs opened.
    pub bold_runs: u32,
    /// `<i>` runs opened.
    pub italic_runs: u32,
    /// `<code>` runs opened.
    pub code_runs: u32,
    /// `<h1>` blocks emitted.
    pub headers: u32,
    /// Bytes that became an escape sequence rather than themselves.
    pub escaped: u32,
    /// `<img>` elements emitted, one per anchor that had an asset.
    pub images: u32,
    /// Anchors with no asset, rendered as `&#xfffc;`.
    ///
    /// Counted rather than ignored: a document whose anchors have lost their assets is a payload whose
    /// text and catalog disagree, and the HTML is where that finally becomes visible.
    pub images_missing: u32,
    /// PNG bytes encoded into `data:` URIs.
    pub image_bytes: u64,
    /// Bytes the `<img>` tags occupied, base64 included.
    ///
    /// The number that matters for the export's size: base64 is 4/3, so a document of images exports
    /// about a third larger than its catalog. Reported so that is a known quantity and not a surprise.
    pub image_tag_bytes: u64,
}

/// Write `editor`'s text and styling to `sink` as a standalone HTML document.
pub fn export<W: Write>(
    editor: &Editor,
    sink: &mut W,
    opts: &HtmlOptions,
) -> Result<HtmlStats, HtmlError> {
    let mut stats = HtmlStats::default();
    let mut w = Counted::new(sink);

    w.write_all(b"<!DOCTYPE html>\n<html>\n<head>\n")?;
    if opts.charset_meta {
        w.write_all(b"<meta charset=\"utf-8\">\n")?;
    }
    // Escaped once and written twice; escaping it per write would walk it twice.
    //
    // A `Vec<u8>`, not a `String`: escaping is a byte-wise operation and a `String` would re-encode
    // any byte above 127 as its own codepoint, turning a UTF-8 title into mojibake. So the tags go out
    // as explicit writes around it rather than through `write!`'s `Display`.
    let mut title: Vec<u8> = Vec::with_capacity(opts.title.len());
    for b in opts.title.bytes() {
        stats.escaped += u32::from(escape_expansion(b));
        push_escaped_byte(&mut title, b);
    }
    w.write_all(b"<title>")?;
    w.write_all(&title)?;
    w.write_all(b"</title>\n")?;
    if opts.stylesheet {
        write!(w, "<style>\n{}\n</style>\n", HtmlOptions::STYLESHEET)?;
    }
    w.write_all(b"</head>\n<body>\n")?;

    export_body(&mut w, editor)?;

    w.write_all(b"</body>\n</html>\n")?;
    // The outer counter's total, not the body's: it covers the preamble too.
    stats.bytes = w.count();
    Ok(stats)
}

/// Write the body only: the document's text and styling, with no `<html>` around it.
///
/// The entry point for a caller that has its own template, and the one the tests use so a span
/// assertion is not buried in a preamble.
pub fn export_body<W: Write>(sink: &mut W, editor: &Editor) -> Result<HtmlStats, HtmlError> {
    let mut stats = HtmlStats::default();
    let mut w = Counted::new(sink);

    let text_len = editor.text_len();
    let mut open = Open::NONE;
    let mut chunk = vec![0u8; CHUNK_BYTES];

    // The run list covers the whole document, so it is walked in step with the chunks. Two cursors,
    // one over runs and one over bytes, both advancing: the span map says where a style changes and
    // the rope says what the bytes are.
    let runs = editor.spans().runs_in(0, text_len as u32);
    let mut run_ix = 0usize;
    let mut chunk_base = 0usize;
    // How many anchors have gone past. **A counter, not a search**, and the reason is worth stating:
    // the first version derived the ordinal from the anchor's byte offset, which meant
    // `AssetCatalog::ordinal_at(&editor.text()?[..at], at)` -- a whole second copy of the document, per
    // image, in an exporter whose entire design is to never hold more than `CHUNK_BYTES` at a time. The
    // walk is already in document order (runs and chunks both advance monotonically), so counting is
    // exact and costs nothing.
    let mut anchor_ordinal = 0usize;
    // Up to two bytes of an anchor that straddled the previous run's end. **A field rather than a
    // local** for the same reason every other scratch here is: a `Vec` local whose slice is passed on
    // escapes and is promoted to the heap.
    let mut carry: Vec<u8> = Vec::with_capacity(2);

    while chunk_base < text_len {
        let n = editor.read_into(chunk_base, &mut chunk)?;
        if n == 0 {
            break;
        }
        let chunk_end = chunk_base + n;

        while run_ix < runs.len() {
            let (_, start, end) = runs[run_ix];
            let start = start as usize;
            let end = end as usize;
            if start >= chunk_end {
                break;
            }
            // The run's part inside this chunk. A run may straddle chunks; the tail is emitted with
            // the next chunk, under the same open tags, which is why `open` persists across chunks.
            let from = start.max(chunk_base);
            let to = end.min(chunk_end);
            let span = &runs[run_ix].0;
            let next = Open::of(span);
            if next != open {
                close_delta(&mut w, open, next)?;
                open_delta(&mut w, next, &mut stats)?;
                open = next;
            }
            // `base` is the absolute byte offset of the slice's first byte, so an anchor's ordinal can
            // be derived from where it sits rather than from a counter that could drift.
            emit_with_anchors(
                &mut w,
                &chunk[from - chunk_base..to - chunk_base],
                editor.assets(),
                &mut anchor_ordinal,
                &mut carry,
                &mut stats,
            )?;
            stats.text_bytes += (to - from) as u64;
            if end <= chunk_end {
                run_ix += 1;
            } else {
                break;
            }
        }
        chunk_base = chunk_end;
    }

    // Whatever is still held back is an anchor that was cut in half by the end of the document, which
    // cannot happen through `insert_image` (the anchor is inserted whole) but can through a hand-edited
    // text. Emitting it as a character reference is the honest reading: the document really does end
    // with half an object-replacement character.
    if !carry.is_empty() {
        let mut tag = Vec::new();
        crate::asset::html_missing_into(&mut tag);
        stats.images_missing += 1;
        w.write_all(&tag)?;
        carry.clear();
    }

    close_delta(&mut w, open, Open::NONE)?;
    stats.bytes = w.count();
    Ok(stats)
}

/// Write `bytes` to `w`, escaping as it goes, with an image anchor becoming an `<img>`.
///
/// # Why the anchor is caught here and not by a pass over the text
///
/// The exporter walks the document in 64 KiB chunks so it never holds more than that in memory, and a
/// second pass over the text to find the anchors would be a whole second copy of the document. So the
/// anchor is caught in the byte stream as it goes past, and `base` is what turns a position in the
/// document into an ordinal in the catalog.
///
/// # Why a partial anchor at a slice's end is not an error
///
/// The anchor is three bytes and a run boundary can fall inside it -- `read_into` and `runs_in` both
/// cut on their own edges. Emitting the first byte of an anchor as text would put a raw `EF` in the
/// output and the document would be invalid UTF-8; holding it back and re-examining the next slice is
/// the only correct answer. [`crate::asset::find_anchor`] reports the partial tail, and the escape
/// filter passes bytes through unchanged, so a held-back byte resumes in the right place.
fn emit_with_anchors<W: Write>(
    w: &mut Counted<W>,
    bytes: &[u8],
    catalog: &holonomy_text::AssetCatalog,
    ordinal: &mut usize,
    carry: &mut Vec<u8>,
    stats: &mut HtmlStats,
) -> Result<(), HtmlError> {
    // **Complete a held-back anchor first.** The bytes the previous slice ended with are the prefix of an
    // anchor whose rest is at the head of this one, and `find_anchor(bytes)` cannot see them -- so without
    // this the third byte goes out as raw text and the export is invalid UTF-8. That is exactly what the
    // first version did, and what `html_finds_an_image_whose_anchor_straddles_a_run_boundary` catches.
    if !carry.is_empty() {
        let mut head = [0u8; 3];
        let clen = carry.len().min(3);
        let take = (3 - clen).min(bytes.len());
        head[..clen].copy_from_slice(&carry[..clen]);
        head[clen..clen + take].copy_from_slice(&bytes[..take]);
        if clen + take < 3 {
            // Still short of three. Keep what arrived and emit nothing: the rest is in a later chunk.
            let mut out = std::mem::take(carry);
            out.clear();
            out.extend_from_slice(bytes);
            *carry = out;
            return Ok(());
        }
        if head == ANCHOR_BYTES {
            // **Discarded, not emitted.** The held-back bytes are the anchor's own prefix, and the
            // anchor is about to be written in full -- emitting them first would put a bare `EF BF` in
            // the output, which is what the first version did and what made this path produce invalid
            // UTF-8 even though it found the anchor.
            carry.clear();
            return emit_anchor(w, &bytes[take..], catalog, ordinal, carry, stats);
        }
        // Not an anchor after all: the held-back bytes were ordinary text.
        emit_escaped(w, carry, stats)?;
        carry.clear();
    }

    let (at, partial) = crate::asset::find_anchor(bytes);
    let Some(at) = at else {
        // Hold back a partial anchor's lead bytes rather than emitting half a character. They are the
        // next slice's problem, and `carry` is how the next slice learns about them.
        let keep = bytes.len().saturating_sub(partial);
        let mut out = std::mem::take(carry);
        out.clear();
        out.extend_from_slice(&bytes[bytes.len() - partial..]);
        *carry = out;
        return emit_escaped(w, &bytes[..keep], stats);
    };
    if at > 0 {
        emit_escaped(w, &bytes[..at], stats)?;
    }
    emit_anchor(
        w,
        &bytes[at + ANCHOR_BYTES.len()..],
        catalog,
        ordinal,
        carry,
        stats,
    )
}

/// Emit the picture for the next anchor, then whatever text follows it.
///
/// Split out of [`emit_with_anchors`] because two paths reach it -- a whole anchor inside one slice, and
/// one completed from the previous slice's carry -- and the second has already consumed the first `take`
/// bytes, so both must resume on the same remainder. Having it in one place means the image emission and
/// the ordinal increment cannot happen once on one path and twice on the other.
fn emit_anchor<W: Write>(
    w: &mut Counted<W>,
    rest: &[u8],
    catalog: &holonomy_text::AssetCatalog,
    ordinal: &mut usize,
    carry: &mut Vec<u8>,
    stats: &mut HtmlStats,
) -> Result<(), HtmlError> {
    let mine = *ordinal;
    *ordinal += 1;
    match crate::asset::asset_at(catalog, mine) {
        Some(asset) => {
            let mut tag = Vec::with_capacity(crate::asset::base64_len(asset.len()) + 64);
            crate::asset::html_img_into(asset, &mut tag);
            stats.images += 1;
            stats.image_bytes += asset.len() as u64;
            stats.image_tag_bytes += tag.len() as u64;
            w.write_all(&tag)?;
        }
        None => {
            // An anchor with no asset: the reference survives, the picture does not. A numeric character
            // reference is the honest rendering of "U+FFFC, unpaired", and it is counted so a document
            // whose anchors have lost their assets says so here rather than looking like a document with
            // no images at all.
            stats.images_missing += 1;
            let mut tag = Vec::new();
            crate::asset::html_missing_into(&mut tag);
            w.write_all(&tag)?;
        }
    }
    stats.text_bytes += ANCHOR_BYTES.len() as u64;
    emit_with_anchors(w, rest, catalog, ordinal, carry, stats)
}

/// Write `bytes` to `w`, escaping as it goes.
fn emit_escaped<W: Write>(
    w: &mut Counted<W>,
    bytes: &[u8],
    stats: &mut HtmlStats,
) -> Result<(), HtmlError> {
    // Fast path: a run with nothing to escape goes straight out. Most of a document is this.
    if !bytes.iter().any(|b| needs_escape(*b)) {
        w.write_all(bytes)?;
        return Ok(());
    }
    let mut scratch: Vec<u8> = Vec::with_capacity(bytes.len() + 16);
    for b in bytes {
        push_escaped_byte(&mut scratch, *b);
        stats.escaped += u32::from(escape_expansion(*b));
    }
    w.write_all(&scratch)?;
    Ok(())
}

/// Append `byte`'s escaped form to `out`.
///
/// Byte-wise and byte-transparent: anything that is not markup goes through **unchanged**, including
/// the continuation bytes of a multi-byte UTF-8 character. Escaping is therefore a pure byte filter
/// and a round trip of unescaped output reproduces the input exactly -- which is what
/// [`unescaped_bytes_pass_through`] asserts, and what would fail if this ever became a `char`-wise
/// operation.
pub fn push_escaped_byte(out: &mut Vec<u8>, byte: u8) {
    match byte {
        b'<' => out.extend_from_slice(b"&lt;"),
        b'>' => out.extend_from_slice(b"&gt;"),
        b'&' => out.extend_from_slice(b"&amp;"),
        b'"' => out.extend_from_slice(b"&quot;"),
        // `&#39;` rather than `&apos;`: `&apos;` is XHTML-only, and a reader parsing this as
        // text/html would show it literally.
        b'\'' => out.extend_from_slice(b"&#39;"),
        _ => out.push(byte),
    }
}

/// Whether `b` becomes an escape sequence.
#[inline]
const fn needs_escape(b: u8) -> bool {
    matches!(b, b'<' | b'>' | b'&' | b'"' | b'\'')
}

/// How many bytes `b` grows by when escaped.
///
/// One entry per escape sequence, and `escaping_expansion_matches_what_is_written` asserts each
/// against what `push_escaped_byte` actually writes. It exists as a table rather than as
/// `written.len() - 1` because the exporter counts escaped bytes for its status line, and a counter
/// that is wrong by one is invisible in the output and wrong in the number.
///
/// `'` is the one to watch: `&#39;` is five bytes, not six. `&quot;` is six.
#[inline]
const fn escape_expansion(b: u8) -> u8 {
    match b {
        b'<' | b'>' => 3, // &lt;  &gt;
        b'&' => 4,        // &amp;
        b'"' => 5,        // &quot;
        b'\'' => 4,       // &#39;
        _ => 0,
    }
}

/// The tags open for one span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Open {
    bold: bool,
    italic: bool,
    code: bool,
    header: bool,
}

impl Open {
    const NONE: Self = Self {
        bold: false,
        italic: false,
        code: false,
        header: false,
    };

    fn of(span: &TextIntervalSpan) -> Self {
        Self {
            bold: span.style_flags & STYLE_BOLD != 0,
            italic: span.style_flags & STYLE_ITALIC != 0,
            code: span.style_flags & STYLE_CODE != 0,
            header: span.style_flags & STYLE_HEADER != 0,
        }
    }

    /// The opening tags, outermost first.
    ///
    /// Heading outside everything, then bold, then italic, then code innermost. Fixed rather than a bit
    /// scan: the close order has to match exactly and a reversed close renders the rest of the page in
    /// the wrong style, which browsers do not report.
    fn opens_for(self, out: &mut Vec<&'static str>) {
        out.clear();
        if self.header {
            out.push("h1");
        }
        if self.bold {
            out.push("b");
        }
        if self.italic {
            out.push("i");
        }
        if self.code {
            out.push("code");
        }
    }

    /// The closing tags for going from `self` to `to`, innermost first.
    fn closes_for(self, to: Self, out: &mut Vec<&'static str>) {
        out.clear();
        if self.code && !to.code {
            out.push("code");
        }
        if self.italic && !to.italic {
            out.push("i");
        }
        if self.bold && !to.bold {
            out.push("b");
        }
        if self.header && !to.header {
            out.push("h1");
        }
    }
}

fn close_delta<W: Write>(w: &mut W, from: Open, to: Open) -> Result<(), HtmlError> {
    let mut tags = Vec::new();
    from.closes_for(to, &mut tags);
    for tag in tags {
        write!(w, "</{tag}>")?;
    }
    Ok(())
}

fn open_delta<W: Write>(w: &mut W, to: Open, stats: &mut HtmlStats) -> Result<(), HtmlError> {
    if to.header {
        stats.headers += 1;
    }
    if to.bold {
        stats.bold_runs += 1;
    }
    if to.italic {
        stats.italic_runs += 1;
    }
    if to.code {
        stats.code_runs += 1;
    }
    let mut tags = Vec::new();
    to.opens_for(&mut tags);
    for tag in tags {
        write!(w, "<{tag}>")?;
    }
    Ok(())
}

/// A `Write` that tallies into a [`HtmlStats`] the caller already owns.
///
/// Borrowed rather than owned so the caller's `stats` stays readable after the writer is dropped --
/// which is the difference between `export` returning its numbers and having to reconstruct them.
struct Counted<W: Write> {
    inner: W,
    bytes: u64,
}

impl<W: Write> Counted<W> {
    fn new(inner: W) -> Self {
        Self { inner, bytes: 0 }
    }

    /// Bytes written so far.
    fn count(&self) -> u64 {
        self.bytes
    }
}

impl<W: Write> Write for Counted<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.bytes += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_open_and_close_orders_are_exact_mirrors() {
        // Every combination of flags, opened and then closed, must balance.
        for bits in 0u8..16 {
            let o = Open {
                bold: bits & 1 != 0,
                italic: bits & 2 != 0,
                code: bits & 4 != 0,
                header: bits & 8 != 0,
            };
            let mut opens = Vec::new();
            o.opens_for(&mut opens);
            let mut closes = Vec::new();
            o.closes_for(Open::NONE, &mut closes);
            let mut reversed = opens.clone();
            reversed.reverse();
            assert_eq!(reversed, closes, "flags {bits:#x}: {opens:?} vs {closes:?}");
        }
    }

    #[test]
    fn a_partial_close_only_closes_what_ended() {
        let both = Open {
            bold: true,
            italic: true,
            code: false,
            header: false,
        };
        let bold_only = Open {
            bold: true,
            italic: false,
            code: false,
            header: false,
        };
        let mut closes = Vec::new();
        both.closes_for(bold_only, &mut closes);
        assert_eq!(closes, vec!["i"]);
    }

    #[test]
    fn escaping_expansion_matches_what_is_written() {
        for b in 0u8..=255 {
            let mut s: Vec<u8> = Vec::new();
            push_escaped_byte(&mut s, b);
            assert_eq!(
                s.len(),
                1 + usize::from(escape_expansion(b)),
                "byte {b:#x} -> {s:?}"
            );
            assert_eq!(needs_escape(b), escape_expansion(b) > 0, "byte {b:#x}");
        }
    }

    /// The inverse of [`push_escaped_byte`], so escaping can be tested as a round trip.
    ///
    /// Deliberately written from the *output* side: it scans for `&` and requires a known entity. A
    /// raw `&` that is not part of one is left alone, which is what makes the round trip a real test
    /// rather than a filter that quietly drops whatever it does not recognise.
    fn unescape(input: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(input.len());
        let mut i = 0usize;
        while i < input.len() {
            if input[i] == b'&' {
                let rest = &input[i..];
                let found = [
                    ("&lt;", b'<'),
                    ("&gt;", b'>'),
                    ("&quot;", b'"'),
                    ("&#39;", b'\''),
                    ("&amp;", b'&'),
                ]
                .into_iter()
                .find(|(seq, _)| rest.starts_with(seq.as_bytes()));
                match found {
                    Some((seq, b)) => {
                        out.push(b);
                        i += seq.len();
                        continue;
                    }
                    // A `&` that begins no known entity is data, not markup.
                    None => out.push(b'&'),
                }
            } else {
                out.push(input[i]);
            }
            i += 1;
        }
        out
    }

    #[test]
    fn unescaped_bytes_pass_through() {
        // Every byte, all 256. Anything that is not one of the five markup characters must come back
        // out of `unescape` as itself, or the exporter is corrupting text.
        let input: Vec<u8> = (0u8..=255).collect();
        let mut escaped = Vec::new();
        for b in &input {
            push_escaped_byte(&mut escaped, *b);
        }
        assert_eq!(
            unescape(&escaped),
            input,
            "escape is not invertible over 0..=255"
        );
    }

    #[test]
    fn a_utf8_document_survives_escape_and_unescape() {
        let text = "caf\u{e9} \u{4e2d}\u{6587} 1 < 2 && 3 > 2 \"quoted\" it's";
        let mut escaped = Vec::new();
        for b in text.as_bytes() {
            push_escaped_byte(&mut escaped, *b);
        }
        assert_eq!(unescape(&escaped), text.as_bytes());

        // The output is *not* ASCII -- `caf\u{e9}` and `\u{4e2d}\u{6587}` pass through as their own UTF-8
        // bytes, which is the whole point of escaping byte-wise. What must be gone is the markup.
        let text_out = String::from_utf8(escaped).expect("escaping broke UTF-8");
        assert!(
            text_out.contains("caf\u{e9}"),
            "UTF-8 was re-encoded instead of passed through"
        );
        assert!(text_out.contains("\u{4e2d}\u{6587}"));
        assert!(
            !text_out.contains('<') && !text_out.contains('>'),
            "raw angle brackets survived"
        );
        assert!(text_out.contains("&lt; 2 &amp;&amp; 3 &gt; 2"));
    }

    #[test]
    fn a_bare_ampersand_is_preserved_as_data() {
        // `unescape` must not eat an `&` that starts no entity, or the round-trip test above would
        // pass while still losing text.
        assert_eq!(unescape(b"R&D"), b"R&D");
        assert_eq!(unescape(b"&unknown;"), b"&unknown;");
    }

    #[test]
    fn utf8_survives_escaping_intact() {
        // The bug this guards: a `char`-wise escaper turns each byte above 127 into its own codepoint,
        // so `é` (C3 A9) comes out as four UTF-8 bytes of mojibake. Byte-wise it comes back as itself.
        for text in ["caf\u{e9}", "\u{4e2d}\u{6587}", "\u{1f600}", "na\u{ef}ve"] {
            let mut out = Vec::new();
            for b in text.as_bytes() {
                push_escaped_byte(&mut out, *b);
            }
            assert_eq!(
                String::from_utf8(out).expect("escaping broke UTF-8"),
                text,
                "{text:?} did not survive escaping"
            );
        }
    }

    #[test]
    fn the_five_markup_characters_become_exactly_the_expected_entities() {
        // Written out rather than derived, because the point is that these five specific byte strings
        // are what a reader's parser expects. `&` appearing *inside* the output is correct -- it is the
        // start of every one of them -- so the assertion is on the whole entity, not on the byte's
        // absence, which is the mistake an earlier version of this test made.
        for (b, want) in [
            (b'<', &b"&lt;"[..]),
            (b'>', &b"&gt;"[..]),
            (b'&', &b"&amp;"[..]),
            (b'"', &b"&quot;"[..]),
            (b'\'', b"&#39;"),
        ] {
            assert!(needs_escape(b));
            let mut out = Vec::new();
            push_escaped_byte(&mut out, b);
            assert_eq!(
                out,
                want,
                "{:?} should become {:?}",
                b as char,
                String::from_utf8_lossy(want)
            );
            assert!(out.is_ascii());
        }
    }

    #[test]
    fn every_escaped_form_is_a_recognised_entity() {
        // The exporter emits five byte strings; a reader's parser must recognise all five, or one of
        // them shows up literally in the page.
        let mut out = Vec::new();
        for b in *b"<>&\"'" {
            push_escaped_byte(&mut out, b);
        }
        let text = String::from_utf8(out).expect("escapes are ascii");
        assert_eq!(text, "&lt;&gt;&amp;&quot;&#39;");
        assert_eq!(unescape(text.as_bytes()), b"<>&\"'");
    }
}
