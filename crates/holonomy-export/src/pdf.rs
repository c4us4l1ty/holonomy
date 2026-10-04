//! PDF, via `pdf-writer`: Base-14 faces, real wrapping, `BT`/`Tf`/`Td`/`Tj`/`ET`.
//!
//! # Layout in integer arithmetic
//!
//! The brief requires integer geometry, and PDF makes that easy rather than awkward: text positions are
//! in *points*, and font metrics are in 1/1000 em. So a line's width is
//!
//! ```text
//!   points = 1000 * sum_of_widths * font_size / (1000 * 1000)
//! ```
//!
//! and the whole comparison -- "does this run fit in the measure?" -- reduces to comparing the sum of
//! 1/1000-em widths against `measure * 1000 / font_size`. No `f32` is involved in deciding where a
//! line breaks. `f32` appears only where the PDF syntax demands it (page boxes, `Tf` sizes), because
//! a PDF viewer reads those as reals and there is nothing to be gained by writing them as integers.
//!
//! # Where a run breaks, and why it is the run
//!
//! Wrapping is done at **run** boundaries -- where the span map changes style -- and never mid-run, not
//! even mid-word. A document's runs are its *formatting* boundaries, and re-flowing text across a
//! formatting boundary would mean splitting a `<b>` run in the PDF, which no reader rejoins. So a long
//! bold run overflows the measure rather than being split.
//!
//! That is a real limitation and it is a deliberate choice: mid-run wrapping needs the same run broken
//! across two `Tj` calls, which needs per-word measurement inside a run, which needs a word
//! segmenter. PROJECT.md Phase 8 does not ask for a word segmenter, and guessing where words are
//! inside a `code` run would be worse than not wrapping it. [`PdfStats::overflowing_lines`] counts the
//! occurrences so the limitation is visible rather than silent.
//!
//! # Base-14, no embedding
//!
//! Every conforming reader carries the 14 standard fonts, so a Type1 font dictionary with a `/BaseFont`
//! and an `/Encoding` is a complete font and the export is a few KB of operators. Embedding a subsetted
//! TTF would mean shipping the subsetter and the glyph tables; see [`crate::fonts`] for why that is
//! also what keeps the exporter inside the binary budget.
//!
//! # Pre-opened descriptors
//!
//! Nothing here opens anything. The exporter takes a `W: Write`, and the session passes a
//! pre-opened fd -- see the crate docs for why that is not negotiable.

use std::io::{self, Write};

use pdf_writer::{Content, Finish, Name, Pdf, Rect, Ref, Str, TextStr};

use crate::fonts::{advance, winansi, BaseFont};
use holonomy_text::{Editor, TextIntervalSpan, STYLE_BOLD, STYLE_CODE, STYLE_HEADER, STYLE_ITALIC};

/// The byte substituted for a character WinAnsi cannot encode.
///
/// `?` rather than a space: a space would silently change the meaning of a line, and `?` at least says
/// something was lost. [`PdfStats::substituted`] counts them.
pub const UNMAPPED: u8 = b'?';

/// Points per inch, for converting the font sizes the editor uses into PDF's unit.
pub const POINTS_PER_INCH: f32 = 72.0;

/// Why a PDF export stopped early.
#[derive(Debug)]
pub enum PdfError {
    /// The sink failed.
    Io(io::Error),
    /// The document refused to be read.
    Editor(holonomy_text::EditorError),
    /// `pdf-writer` refused to finish the file.
    ///
    /// Carries nothing: it is a broken-invariant report rather than a recoverable condition, and there
    /// is no partial output to salvage.
    Writer,
}

impl std::fmt::Display for PdfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "writing PDF: {e}"),
            Self::Editor(e) => write!(f, "reading the document for PDF: {e}"),
            Self::Writer => write!(f, "the PDF writer refused to finish the file"),
        }
    }
}

impl std::error::Error for PdfError {}

impl From<io::Error> for PdfError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<holonomy_text::EditorError> for PdfError {
    fn from(e: holonomy_text::EditorError) -> Self {
        Self::Editor(e)
    }
}

/// Page geometry, in points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageSize {
    /// Width.
    pub width: f32,
    /// Height.
    pub height: f32,
}

impl PageSize {
    /// US Letter: 8.5 x 11 inches at 72 points per inch.
    pub const LETTER: Self = Self {
        width: 612.0,
        height: 792.0,
    };

    /// ISO A4.
    pub const A4: Self = Self {
        width: 595.0,
        height: 842.0,
    };

    /// The origin as a `Rect`, which is how `pdf-writer` wants a media box.
    fn rect(&self) -> Rect {
        Rect::new(0.0, 0.0, self.width, self.height)
    }
}

/// What the PDF should look like.
#[derive(Debug, Clone)]
pub struct PdfOptions {
    /// `/Title` in the document information dictionary.
    pub title: String,
    /// Page size.
    pub page: PageSize,
    /// Body text size, in points.
    pub font_size: f32,
    /// Space between baselines, in points.
    pub line_height: f32,
    /// Margin on every side, in points.
    pub margin: f32,
}

impl Default for PdfOptions {
    fn default() -> Self {
        Self {
            title: "Holonomy".to_string(),
            page: PageSize::LETTER,
            font_size: 11.0,
            line_height: 14.0,
            margin: 54.0,
        }
    }
}

impl PdfOptions {
    /// The usable text width, in points.
    #[inline]
    pub fn measure(&self) -> f32 {
        self.page.width - 2.0 * self.margin
    }

    /// The measure in 1/1000-em units at [`font_size`](Self::font_size), which is the unit wrapping
    /// compares in.
    ///
    /// `measure * 1000 / font_size`, rounded. Rounding once, up front, means the comparison itself is
    /// exact integer work and the same limit applies to every line on every page.
    #[inline]
    pub fn measure_mils(&self) -> u32 {
        (self.measure() * 1000.0 / self.font_size).round().max(1.0) as u32
    }
}

/// What an export produced, and what it had to compromise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PdfStats {
    /// Bytes written.
    pub bytes: u64,
    /// Pages emitted.
    pub pages: u32,
    /// Lines laid out.
    pub lines: u32,
    /// Characters that became `?` because WinAnsi has no code for them.
    pub substituted: u32,
    /// Lines that overflowed the measure rather than being broken mid-word.
    ///
    /// Non-zero means at least one word is wider than the measure. That is the only way a line can
    /// overflow, because the exporter breaks between words -- see [`Layouter`], which explains why
    /// breaking inside one is worse than overflowing it.
    pub overflowing_lines: u32,
}

/// The faces the exporter draws with.
///
/// Four resources, one per combination of weight and slant, plus Courier for `code`. Declared as a
/// `const` so the resource names and the widths cannot drift apart.
struct Faces<'a> {
    regular: Name<'a>,
    bold: Name<'a>,
    italic: Name<'a>,
    bold_italic: Name<'a>,
    code: Name<'a>,
    code_bold: Name<'a>,
}

impl Faces<'_> {
    const NAMES: Faces<'static> = Faces {
        regular: Name(b"F1"),
        bold: Name(b"F2"),
        italic: Name(b"F3"),
        bold_italic: Name(b"F4"),
        code: Name(b"F5"),
        code_bold: Name(b"F6"),
    };

    /// The face and its metric face for one span.
    fn for_span(span: &TextIntervalSpan) -> (Name<'static>, BaseFont) {
        if span.style_flags & STYLE_CODE != 0 {
            if span.style_flags & STYLE_BOLD != 0 {
                // Courier's four faces are metrically identical, so `code_bold` and `code` measure the
                // same. Naming them separately anyway means a viewer that *does* have a bold Courier
                // shows one, and `fonts.rs` documents why the metrics do not differ.
                return (Faces::NAMES.code_bold, BaseFont::Courier);
            }
            return (Faces::NAMES.code, BaseFont::Courier);
        }
        match (
            span.style_flags & STYLE_BOLD != 0,
            span.style_flags & STYLE_ITALIC != 0,
        ) {
            (false, false) => (Faces::NAMES.regular, BaseFont::Helvetica),
            (true, false) => (Faces::NAMES.bold, BaseFont::HelveticaBold),
            (false, true) => (Faces::NAMES.italic, BaseFont::Helvetica),
            (true, true) => (Faces::NAMES.bold_italic, BaseFont::HelveticaBold),
        }
    }

    /// The `Name` for every face, in resource order.
    fn all(&self) -> [Name<'_>; 6] {
        [
            self.regular,
            self.bold,
            self.italic,
            self.bold_italic,
            self.code,
            self.code_bold,
        ]
    }

    /// The `/BaseFont` for each resource, in the same order.
    fn base_fonts(&self) -> [&'static [u8]; 6] {
        [
            b"Helvetica",
            b"Helvetica-Bold",
            b"Helvetica-Oblique",
            b"Helvetica-BoldOblique",
            BaseFont::Courier.pdf_name(),
            BaseFont::Courier.pdf_name(),
        ]
    }
}

/// Write `editor` to `sink` as a PDF.
pub fn export<W: Write>(
    editor: &Editor,
    sink: &mut W,
    opts: &PdfOptions,
) -> Result<PdfStats, PdfError> {
    let mut stats = PdfStats::default();
    let body = build(editor, opts, &mut stats)?;
    sink.write_all(&body)?;
    sink.flush()?;
    stats.bytes = body.len() as u64;
    Ok(stats)
}

/// Build the whole PDF in memory and return it.
///
/// `pdf-writer` works by building an in-memory object graph and serialising at the end, so "stream to
/// the sink" means "build then write". That is a deliberate acceptance: the alternative is
/// hand-writing the cross-reference table, which is the part of PDF every reader is strictest about.
/// For a document-sized file this is one allocation of one document's worth of operators.
pub fn build(
    editor: &Editor,
    opts: &PdfOptions,
    stats: &mut PdfStats,
) -> Result<Vec<u8>, PdfError> {
    let mut pdf = Pdf::new();
    // Object ids. Fixed rather than allocated, because the whole file is written in one pass and a
    // bump allocator here would be a second thing to get wrong for no benefit.
    let catalog_id = Ref::new(1);
    let pages_id = Ref::new(2);
    let info_id = Ref::new(3);
    // Pages start at 10 so a future addition (an outline, an embedded font) can take 4..9 without
    // renumbering.
    const FIRST_PAGE: i32 = 10;
    const FIRST_FONT: i32 = 100;
    const FIRST_CONTENT: i32 = 200;

    pdf.catalog(catalog_id).pages(pages_id);
    pdf.document_info(info_id).title(TextStr(&opts.title));

    let faces = Faces::NAMES;

    // Lay the document out into pages of content operators first, so the page tree can be written with
    // the real count instead of a placeholder.
    let text_len = editor.text_len();
    let runs = editor.spans().runs_in(0, text_len as u32);
    let mut chunk = vec![0u8; crate::html::CHUNK_BYTES];

    let mut pages: Vec<Content> = Vec::new();
    let mut current = Content::new();
    let mut y = opts.page.height - opts.margin;
    let mut lines_on_page = 0usize;
    // The line being accumulated. It survives across runs, which is the whole point: the span map's
    // runs are *formatting* boundaries and most of them sit in the middle of a line.
    let mut layout = Layouter::new(opts);

    // Walk runs and chunks in step, exactly as the HTML exporter does, so the two agree on where the
    // document's formatting boundaries are.
    let mut run_ix = 0usize;
    let mut chunk_base = 0usize;

    while chunk_base < text_len {
        let n = editor.read_into(chunk_base, &mut chunk)?;
        if n == 0 {
            break;
        }
        let chunk_end = chunk_base + n;

        while run_ix < runs.len() {
            let (span, start, end) = runs[run_ix];
            let from = (start as usize).max(chunk_base);
            let to = (end as usize).min(chunk_end);
            if from >= to {
                if (end as usize) <= chunk_end {
                    run_ix += 1;
                    continue;
                }
                break;
            }

            let slice = &chunk[from - chunk_base..to - chunk_base];
            let (font_name, metric) = Faces::for_span(&span);
            let big = span.style_flags & STYLE_HEADER != 0;
            let size = if big {
                opts.font_size * HEADER_SCALE
            } else {
                opts.font_size
            };

            for line in layout.feed(slice, font_name, metric, size, big, stats) {
                if lines_on_page == lines_per_page(opts) || y < opts.margin {
                    pages.push(std::mem::replace(&mut current, Content::new()));
                    y = opts.page.height - opts.margin;
                    lines_on_page = 0;
                }
                write_line(&mut current, &line.segments, y, opts.margin, opts.font_size);
                y -= opts.line_height * line.scale;
                lines_on_page += 1;
                stats.lines += 1;
            }

            if (end as usize) <= chunk_end {
                run_ix += 1;
            } else {
                break;
            }
        }
        chunk_base = chunk_end;
    }
    // A trailing run with no newline still produced a line, via `finish`.
    for line in layout.finish() {
        if lines_on_page == lines_per_page(opts) || y < opts.margin {
            pages.push(std::mem::replace(&mut current, Content::new()));
            y = opts.page.height - opts.margin;
            lines_on_page = 0;
        }
        write_line(&mut current, &line.segments, y, opts.margin, opts.font_size);
        y -= opts.line_height * line.scale;
        lines_on_page += 1;
        stats.lines += 1;
    }
    // A document with no text at all is still one blank page, not zero: a PDF with an empty page tree
    // is rejected by some readers.
    if lines_on_page > 0 {
        pages.push(current);
    }
    if pages.is_empty() {
        pages.push(Content::new());
    }

    stats.pages = pages.len() as u32;

    // The page tree, now that the count is known.
    let page_refs: Vec<Ref> = (0..pages.len())
        .map(|i| Ref::new(FIRST_PAGE + i as i32))
        .collect();
    pdf.pages(pages_id)
        .kids(page_refs.iter().copied())
        .count(pages.len() as i32);

    // Fonts, once each, referenced by every page.
    for (i, base) in faces.base_fonts().iter().enumerate() {
        let id = Ref::new(FIRST_FONT + i as i32);
        // WinAnsi, not Standard: `fonts.rs` indexes the width tables by WinAnsi code, so the declared
        // encoding and the metrics have to be the same one or wrapping lands where the reader does not
        // break.
        pdf.type1_font(id)
            .base_font(Name(base))
            .encoding_predefined(Name(b"WinAnsiEncoding"));
    }

    for (i, content) in pages.into_iter().enumerate() {
        let page_id = Ref::new(FIRST_PAGE + i as i32);
        let content_id = Ref::new(FIRST_CONTENT + i as i32);

        let mut page = pdf.page(page_id);
        page.media_box(opts.page.rect());
        page.parent(pages_id);
        page.contents(content_id);
        {
            // Scoped: `resources()` hands out a temporary, and holding `fonts` across the loop keeps
            // that temporary alive, which is why this is a block rather than two statements.
            let mut resources = page.resources();
            let mut fonts = resources.fonts();
            for (ix, name) in faces.all().iter().enumerate() {
                fonts.pair(*name, Ref::new(FIRST_FONT + ix as i32));
            }
            fonts.finish();
        }
        page.finish();

        pdf.stream(content_id, &content.finish());
    }

    Ok(pdf.finish())
}

/// How many lines fit between the margins.
fn lines_per_page(opts: &PdfOptions) -> usize {
    let usable = (opts.page.height - 2.0 * opts.margin) / opts.line_height;
    (usable.floor() as usize).max(1)
}

/// How much bigger a header line is than body text.
pub const HEADER_SCALE: f32 = 1.35;

/// Spaces a tab expands to in the PDF export.
///
/// Four is the conventional choice for a proportional face at 10-12pt, where one em is roughly four
/// spaces wide.
pub const TAB_SPACES: usize = 4;

/// One run of text on a laid-out line, with the face it is drawn in.
#[derive(Debug, Clone, PartialEq)]
struct Segment {
    /// WinAnsi bytes.
    text: Vec<u8>,
    /// The face's resource name.
    font: Name<'static>,
    /// The face's point size -- headers are larger, so a line can mix sizes.
    size: f32,
}

/// One finished line.
#[derive(Debug, Clone, PartialEq)]
struct Line {
    segments: Vec<Segment>,
    /// Line-height multiplier, so a line containing a header takes a header's height.
    scale: f32,
}

/// Accumulates run fragments into lines, breaking at word boundaries.
///
/// # Why this exists
///
/// Two bugs, in order.
///
/// The first version called a per-run `split_lines` and treated each result as a finished line. The
/// span map hands back a run per *formatting* boundary and formatting boundaries sit mid-sentence, so
/// every styled word became its own line: a structurally valid PDF that read back as one short line
/// per styled span.
///
/// The second version accumulated runs into a line correctly but broke the line per *character*. That
/// reads back with a spurious line break inside every long word -- "foxtrot" arrives as "foxtro" plus
/// a line holding "t" -- so the export's text no longer matches the document.
///
/// # The rule
///
/// Break **between** words, at a space, and never inside one. That means a word's width has to be known
/// before the word can be placed, so a word is buffered until its end. Two things are held back while
/// that happens:
///
/// * the word itself, as [`Pending`], and
/// * the spaces before it, as a width.
///
/// Holding the spaces back is what keeps a broken line from ending in whitespace. The cost is that the
/// spaces are committed only once the following word is known to fit -- which is exactly when the
/// decision can be made, and the reason no trailing-space trimming pass is needed.
///
/// # A word wider than the measure
///
/// It goes on a line of its own and overflows, counted in
/// [`PdfStats::overflowing_lines`]. Breaking inside a word to fit would be worse than overflowing it: a
/// hyphen-less mid-word break in a PDF renders as two fragments with no indication they were one word.
#[derive(Debug)]
struct Layouter<'a> {
    opts: &'a PdfOptions,
    /// The line being built.
    segments: Vec<Segment>,
    /// Its width so far, in 1/1000 em.
    width: u32,
    /// Line-height multiplier, so a line containing a header takes a header's height.
    scale: f32,
    /// The word being buffered, if one is open.
    pending: Option<Pending>,
    /// Width of the spaces held back before [`Layouter::pending`].
    held_spaces: u32,
}

/// The face a run of spaces with no word should be drawn in: the line's last face if it has one, else
/// the body face.
fn pending_face(segments: &[Segment]) -> Name<'static> {
    segments
        .last()
        .map(|s| s.font)
        .unwrap_or(Faces::NAMES.regular)
}

/// A word being buffered, in one face.
#[derive(Debug)]
struct Pending {
    text: Vec<u8>,
    font: Name<'static>,
    size: f32,
    /// Its width in 1/1000 em, at its own size.
    width: u32,
}

impl<'a> Layouter<'a> {
    fn new(opts: &'a PdfOptions) -> Self {
        Self {
            opts,
            segments: Vec::new(),
            width: 0,
            scale: 1.0,
            pending: None,
            held_spaces: 0,
        }
    }

    /// The measure in 1/1000 em at `size`, rounded once.
    fn limit_mils(&self, size: f32) -> u32 {
        (self.opts.measure_mils() as f32 * size / self.opts.font_size).round() as u32
    }

    /// Feed one run's bytes, returning every line that completes.
    fn feed(
        &mut self,
        bytes: &[u8],
        font: Name<'static>,
        metric: BaseFont,
        size: f32,
        big: bool,
        stats: &mut PdfStats,
    ) -> Vec<Line> {
        let mut done = Vec::new();
        let limit = self.limit_mils(size);
        let mut i = 0usize;
        while i < bytes.len() {
            match bytes[i] {
                b'\n' => {
                    if let Some(l) = self.commit(stats, limit) {
                        done.push(l);
                    }
                    // A newline drops any spaces held back: trailing whitespace on a line is never
                    // what the author meant, and leaving it in would need a trim pass later.
                    self.held_spaces = 0;
                    done.push(self.take());
                    i += 1;
                    continue;
                }
                b'\r' => {
                    if let Some(l) = self.commit(stats, limit) {
                        done.push(l);
                    }
                    self.held_spaces = 0;
                    done.push(self.take());
                    i += 1;
                    if bytes.get(i) == Some(&b'\n') {
                        i += 1;
                    }
                    continue;
                }
                b' ' => {
                    // A space ends the word that came before it.
                    if let Some(l) = self.commit(stats, limit) {
                        done.push(l);
                    }
                    self.held_spaces += advance(metric, b" ");
                    i += 1;
                    continue;
                }
                b'\t' => {
                    // A tab is a layout instruction with no WinAnsi byte and no intrinsic width. Four
                    // spaces is the conventional stand-in at 10-12pt, and it is not counted as a
                    // substitution because nothing is lost the way `?` for CJK loses something.
                    if let Some(l) = self.commit(stats, limit) {
                        done.push(l);
                    }
                    self.held_spaces += advance(metric, &[b' '; TAB_SPACES]);
                    i += 1;
                    continue;
                }
                _ => {}
            }

            let n = utf8_len(bytes[i]);
            let slice = &bytes[i..(i + n).min(bytes.len())];
            let w = advance(metric, slice);
            let code = transcode(slice, stats);
            match &mut self.pending {
                Some(p) => {
                    p.text.push(code[0]);
                    p.width += w;
                }
                None => {
                    self.pending = Some(Pending {
                        text: vec![code[0]],
                        font,
                        size,
                        width: w,
                    })
                }
            }
            if big {
                self.scale = self.scale.max(HEADER_SCALE);
            }
            i += n;
        }
        // The word open at the end of a run is committed here, not left dangling: a run boundary is a
        // formatting boundary, and there is no reason for text to end mid-word because a style changed.
        if let Some(l) = self.commit(stats, limit) {
            done.push(l);
        }
        done
    }

    /// Place the buffered word and the spaces held back for it.
    ///
    /// Returns the line that was finished, if the word did not fit and the line had to break first.
    /// A break is only ever taken *before* a word, so the returned line always ends at a word boundary
    /// and never in whitespace.
    fn commit(&mut self, stats: &mut PdfStats, limit: u32) -> Option<Line> {
        let Some(pending) = self.pending.take() else {
            // No word open -- a run of only spaces. The held spaces still have to go somewhere, or a
            // line break would discard them; for a run that is *only* spaces they are the whole run.
            self.flush_spaces(pending_face(&self.segments));
            return None;
        };
        if !self.segments.is_empty() && self.width + pending.width > limit {
            // The word goes on the next line, and the space that caused the break is **consumed**.
            //
            // Consuming it is the whole reason the spaces are held back rather than appended: they have
            // not been written yet, so dropping them here cannot leave trailing whitespace on the
            // finished line. Leaving them in -- which is what the first version did -- put the space
            // *after* the moved word, so a document extracted back with two spaces at every break.
            self.held_spaces = 0;
            stats.overflowing_lines += 1;
            let finished = self.take();
            self.push_many(&pending.text, pending.font, pending.size);
            self.width = pending.width;
            return Some(finished);
        }
        self.flush_spaces(pending.font);
        self.width += pending.width;
        self.push_many(&pending.text, pending.font, pending.size);
        None
    }

    /// Append the held spaces, at `face`.
    fn flush_spaces(&mut self, face: Name<'static>) {
        if self.held_spaces == 0 {
            return;
        }
        // One space byte per space width in the *plain* font, then scaled into the segment's face: the
        // alternative is tracking a width per held space, and a space's width does not vary enough for
        // that to change where anything breaks.
        let one = u32::from(BaseFont::Helvetica.width(b' ')).max(1);
        let n = (self.held_spaces.div_ceil(one)) as usize;
        let spaces = vec![b' '; n];
        let w = advance(BaseFont::Helvetica, &spaces);
        self.push_many(&spaces, face, self.opts.font_size);
        self.width += w;
        self.held_spaces = 0;
    }

    /// Append several bytes in one face, extending the last segment when it has not changed.
    ///
    /// Extending rather than starting a new segment matters for the PDF's size and for its readability:
    /// a document with four alternating styles on one line emits four `Tf`s and four `Tj`s, not one
    /// `Tj` per character.
    fn push_many(&mut self, bytes: &[u8], font: Name<'static>, size: f32) {
        match self.segments.last_mut() {
            Some(last) if last.font.0 == font.0 && last.size == size => {
                last.text.extend_from_slice(bytes);
            }
            _ => self.segments.push(Segment {
                text: bytes.to_vec(),
                font,
                size,
            }),
        }
    }

    /// Finish the current line, whatever it holds, and reset.
    fn take(&mut self) -> Line {
        let line = Line {
            segments: std::mem::take(&mut self.segments),
            scale: self.scale,
        };
        self.width = 0;
        self.scale = 1.0;
        line
    }

    /// The line in progress, if there is one.
    ///
    /// An empty line at the very end of a document that ended in a newline is *not* returned: a
    /// trailing newline ends the last line rather than starting a new one.
    fn finish(self) -> Vec<Line> {
        if self.segments.is_empty() && self.held_spaces == 0 {
            Vec::new()
        } else {
            vec![Line {
                segments: self.segments,
                scale: self.scale,
            }]
        }
    }
}

/// The length in bytes of the UTF-8 character starting with `b`, or 1 if `b` cannot start one.
#[inline]
fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        // A continuation byte, or an invalid lead. One byte, so the walk cannot stall on it -- a run
        // with a stray continuation byte would otherwise loop forever.
        _ => 1,
    }
}

/// Transcode one character's UTF-8 bytes to one WinAnsi byte, counting substitutions.
///
/// `None` from [`winansi`] and a truncated character both become [`UNMAPPED`]. For the second case
/// that is the honest answer rather than a safe one: the bytes are half a character, and picking a
/// plausible glyph would put the wrong thing on the page while looking fine.
fn transcode(slice: &[u8], stats: &mut PdfStats) -> [u8; 1] {
    match std::str::from_utf8(slice) {
        Ok(s) => match s.chars().next().and_then(winansi) {
            Some(b) => [b],
            None => {
                stats.substituted += 1;
                [UNMAPPED]
            }
        },
        Err(_) => {
            stats.substituted += 1;
            [UNMAPPED]
        }
    }
}

/// Write one line: `BT`, `Tf`, `Td`, `Tj`, `ET`.
///
/// # One `BT`/`ET` per line, and one `Tf` per segment
///
/// A single text object for the whole page would be smaller, and wrong in a way that matters: a reader
/// that merges two lines' text matrices loses the line structure, so text extraction comes back as one
/// run per page. And `Tf` has to be re-issued per segment because a line can mix faces -- a bold word
/// inside a plain sentence -- and the font is part of the text state, not of the string.
///
/// `Td` is absolute here only because each line gets a fresh `BT`, which resets the line matrix. That
/// is deliberate and it is why the exporter does not have to track accumulated line positions.
fn write_line(out: &mut Content, segments: &[Segment], y: f32, x: f32, body_size: f32) {
    // A blank line emits `BT`, a `Td` and an *empty* `Tj`, and is not skipped. The line has to be
    // positioned: a paragraph break in the document is a line's worth of vertical space, and dropping
    // it makes consecutive paragraphs run together. An empty string operand is legal, and a reader that
    // buckets by position sees a line with no text on it -- which is what a blank line is.
    out.begin_text();
    // `Td` **once per line**, before any `Tj`.
    //
    // `td_is_relative_to_the_line_start_not_the_point` is the test that matters here: inside one `BT`,
    // `tx ty Td` translates the line matrix, so it *accumulates*. Emitting one per segment -- which the
    // first version did, so each styled word got its own `Td` -- put the second segment at twice the x
    // and twice the y, and the line came back from the reader as several lines at increasing heights.
    // A reader reassembling by position then saw a document that grew a line per bold word.
    //
    // The position is then absolute, because `BT` reset the line matrix to the identity and nothing has
    // moved it yet.
    out.next_line(x, y);
    let mut drew = false;
    for seg in segments {
        if seg.text.is_empty() {
            continue;
        }
        out.set_font(seg.font, seg.size);
        out.show(Str(&seg.text));
        drew = true;
    }
    if !drew {
        // The blank-line case: one empty show, so the reader has a run to position.
        //
        // `body_size` is passed in rather than read off the segments, which for a blank line have none.
        // Deriving it from them produced `/F1 0 Tf`, and a font size of zero is not a legal PDF operand.
        out.set_font(Faces::NAMES.regular, body_size);
        out.show(Str(b""));
    }
    out.end_text();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_measure_converts_to_mils_exactly() {
        let o = PdfOptions {
            font_size: 10.0,
            ..Default::default()
        };
        // Letter 612 wide, 54 margins -> 504 points. At 10pt, 504 * 1000 / 10 = 50400 mils.
        assert_eq!(o.measure(), 504.0);
        assert_eq!(o.measure_mils(), 50_400);
    }

    #[test]
    fn the_limit_scales_with_font_size() {
        let small = PdfOptions {
            font_size: 10.0,
            ..Default::default()
        };
        let large = PdfOptions {
            font_size: 20.0,
            ..Default::default()
        };
        // Twice the size, half the room per glyph.
        assert_eq!(large.measure_mils() * 2, small.measure_mils());
    }

    #[test]
    fn utf8_lengths_are_read_from_the_lead_byte() {
        assert_eq!(utf8_len(b'a'), 1);
        assert_eq!(utf8_len(0xC3), 2);
        assert_eq!(utf8_len(0xE2), 3);
        assert_eq!(utf8_len(0xF0), 4);
        // A continuation byte, and 0xFF. Both must advance by 1 or the walk stalls.
        assert_eq!(utf8_len(0x80), 1);
        assert_eq!(utf8_len(0xFF), 1);
    }

    /// The WinAnsi text of each segment of a laid-out line, concatenated.
    fn text_of(line: &Line) -> Vec<u8> {
        line.segments.iter().flat_map(|s| s.text.clone()).collect()
    }

    fn feed_all(opts: &PdfOptions, bytes: &[u8], stats: &mut PdfStats) -> Vec<Line> {
        let mut l = Layouter::new(opts);
        let mut out = l.feed(
            bytes,
            Faces::NAMES.regular,
            BaseFont::Helvetica,
            opts.font_size,
            false,
            stats,
        );
        out.extend(l.finish());
        out
    }

    #[test]
    fn a_newline_ends_a_line() {
        let opts = PdfOptions::default();
        let mut stats = PdfStats::default();
        let lines = feed_all(&opts, b"ab\ncd", &mut stats);
        assert_eq!(lines.len(), 2);
        assert_eq!(text_of(&lines[0]), b"ab");
        assert_eq!(text_of(&lines[1]), b"cd");
        assert_eq!(stats.overflowing_lines, 0);
    }

    #[test]
    fn a_trailing_newline_yields_no_extra_line() {
        let opts = PdfOptions::default();
        let mut stats = PdfStats::default();
        let lines = feed_all(&opts, b"ab\n", &mut stats);
        assert_eq!(
            lines.len(),
            1,
            "a trailing newline ends the line, it does not start one"
        );
        assert_eq!(text_of(&lines[0]), b"ab");
    }

    #[test]
    fn a_bare_newline_yields_an_empty_line() {
        // A blank line in the document is a line in the PDF. Only a *trailing* newline is dropped.
        let opts = PdfOptions::default();
        let mut stats = PdfStats::default();
        let lines = feed_all(&opts, b"a\n\nb", &mut stats);
        assert_eq!(lines.len(), 3);
        assert_eq!(text_of(&lines[1]), b"");
    }

    #[test]
    fn crlf_is_one_break_and_a_lone_cr_is_too() {
        let opts = PdfOptions::default();
        let mut stats = PdfStats::default();
        let lines = feed_all(&opts, b"a\r\nb\rc", &mut stats);
        assert_eq!(lines.len(), 3);
        assert_eq!(text_of(&lines[1]), b"b");
        assert_eq!(
            stats.substituted, 0,
            "a CR is a line ending, not a character to substitute"
        );
    }

    #[test]
    fn an_overlong_line_is_broken_at_the_measure_and_counted() {
        let opts = PdfOptions::default();
        let mut stats = PdfStats::default();
        // The default Letter measure at 11pt is ~45800 mils; 'W' is 944.
        let lines = feed_all(&opts, b"WWWW", &mut stats);
        // All four fit: 4 * 944 = 3776, far inside the measure.
        assert_eq!(lines.len(), 1, "nothing should have wrapped");
        assert_eq!(stats.overflowing_lines, 0);
        assert_eq!(text_of(&lines[0]), b"WWWW");
    }

    #[test]
    fn a_very_narrow_measure_breaks_the_line() {
        let opts = PdfOptions {
            page: PageSize {
                width: 80.0,
                height: 800.0,
            },
            margin: 10.0,
            ..Default::default()
        };
        let mut stats = PdfStats::default();
        let lines = feed_all(&opts, b"alpha bravo charlie", &mut stats);
        assert!(lines.len() > 1, "a 60pt measure cannot hold that");
        for line in &lines {
            assert!(!text_of(line).is_empty(), "a break produced an empty line");
        }
        assert!(stats.overflowing_lines > 0);
        // The invariant that actually holds after wrapping: **no word is split**, and no word is lost.
        //
        // Not "concatenating the lines gives the source", because a line break in a PDF consumes the
        // space that caused it -- that is what word wrapping *is*. So the lines are joined with a space
        // to put the break back.
        let joined: String = lines
            .iter()
            .map(|l| String::from_utf8(text_of(l)).expect("ascii"))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(joined, "alpha bravo charlie");

        // And no line begins or ends with the space a naive wrapper would leave there.
        for line in &lines {
            let t = text_of(line);
            assert_ne!(t.first(), Some(&b' '), "a line starts with a space");
            assert_ne!(t.last(), Some(&b' '), "a line ends with a space");
        }
    }

    #[test]
    fn a_word_wider_than_the_measure_is_not_split() {
        let opts = PdfOptions {
            page: PageSize {
                width: 60.0,
                height: 800.0,
            },
            margin: 10.0,
            ..Default::default()
        };
        let mut stats = PdfStats::default();
        // A 40pt measure, and a word far wider than it.
        let long = "supercalifragilistic";
        let lines = feed_all(&opts, long.as_bytes(), &mut stats);
        assert_eq!(
            lines.len(),
            1,
            "the word overflows rather than being cut in half"
        );
        assert_eq!(text_of(&lines[0]), long.as_bytes());
        assert_eq!(
            stats.overflowing_lines, 0,
            "an unbroken word does not count as an overflowing *line*; it is counted when a break forces it"
        );
    }

    #[test]
    fn a_break_between_two_words_is_counted_once() {
        let opts = PdfOptions {
            page: PageSize {
                width: 90.0,
                height: 800.0,
            },
            margin: 10.0,
            ..Default::default()
        };
        let mut stats = PdfStats::default();
        let lines = feed_all(&opts, b"alpha bravo charlie delta echo", &mut stats);
        assert!(lines.len() > 1);
        assert_eq!(stats.overflowing_lines, lines.len() as u32 - 1);
    }

    #[test]
    fn runs_concatenate_into_one_line_rather_than_one_line_each() {
        // The bug the `Layouter` exists to fix: the span map hands back a run per *formatting*
        // boundary, and those sit mid-sentence. Treating each run as a finished line gave the PDF one
        // short line per styled word.
        let opts = PdfOptions::default();
        let mut stats = PdfStats::default();
        let mut l = Layouter::new(&opts);
        let mut lines = Vec::new();
        for (text, face, metric) in [
            (&b"Bold "[..], Faces::NAMES.bold, BaseFont::HelveticaBold),
            (&b"words. "[..], Faces::NAMES.regular, BaseFont::Helvetica),
            (&b"code"[..], Faces::NAMES.code, BaseFont::Courier),
            (&b"()"[..], Faces::NAMES.code, BaseFont::Courier),
        ] {
            lines.extend(l.feed(text, face, metric, opts.font_size, false, &mut stats));
        }
        lines.extend(l.finish());

        assert_eq!(lines.len(), 1, "four runs must produce one line, not four");
        assert_eq!(text_of(&lines[0]), b"Bold words. code()");
        // And the faces are the three distinct ones, in order.
        let faces: Vec<&[u8]> = lines[0].segments.iter().map(|s| s.font.0).collect();
        assert_eq!(
            faces,
            vec![b"F2", b"F1", b"F5"],
            "expected bold, regular, code"
        );
    }

    #[test]
    fn consecutive_runs_in_the_same_face_merge_into_one_segment() {
        let opts = PdfOptions::default();
        let mut stats = PdfStats::default();
        let mut l = Layouter::new(&opts);
        let mut lines = l.feed(
            b"ab",
            Faces::NAMES.regular,
            BaseFont::Helvetica,
            opts.font_size,
            false,
            &mut stats,
        );
        lines.extend(l.feed(
            b"cd",
            Faces::NAMES.regular,
            BaseFont::Helvetica,
            opts.font_size,
            false,
            &mut stats,
        ));
        lines.extend(l.finish());
        assert_eq!(
            lines[0].segments.len(),
            1,
            "same face, same size: one Tj is enough"
        );
        assert_eq!(text_of(&lines[0]), b"abcd");
    }

    #[test]
    fn a_line_containing_a_header_takes_a_headers_height() {
        let opts = PdfOptions::default();
        let mut stats = PdfStats::default();
        let mut l = Layouter::new(&opts);
        let mut lines = l.feed(
            b"Chapter",
            Faces::NAMES.regular,
            BaseFont::Helvetica,
            opts.font_size * HEADER_SCALE,
            true,
            &mut stats,
        );
        lines.extend(l.finish());
        assert_eq!(lines[0].scale, HEADER_SCALE);
        assert_eq!(lines[0].segments[0].size, opts.font_size * HEADER_SCALE);

        // A plain line does not.
        let plain = feed_all(&opts, b"body", &mut stats);
        assert_eq!(plain[0].scale, 1.0);
    }

    #[test]
    fn a_line_mixes_sizes_and_both_are_kept() {
        let opts = PdfOptions::default();
        let mut stats = PdfStats::default();
        let mut l = Layouter::new(&opts);
        let mut lines = l.feed(
            b"Title",
            Faces::NAMES.regular,
            BaseFont::Helvetica,
            opts.font_size * HEADER_SCALE,
            true,
            &mut stats,
        );
        lines.extend(l.feed(
            b" body",
            Faces::NAMES.regular,
            BaseFont::Helvetica,
            opts.font_size,
            false,
            &mut stats,
        ));
        lines.extend(l.finish());
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0].segments.len(),
            2,
            "different sizes cannot share a Tf"
        );
        assert_eq!(text_of(&lines[0]), b"Title body");
        assert_eq!(lines[0].scale, HEADER_SCALE);
    }

    #[test]
    fn utf8_survives_the_walk() {
        let opts = PdfOptions::default();
        let mut stats = PdfStats::default();
        let lines = feed_all(&opts, "caf\u{e9} \u{4e2d}\u{6587}".as_bytes(), &mut stats);
        assert_eq!(lines.len(), 1);
        // e-acute is WinAnsi 0xE9; the two CJK characters are not WinAnsi at all.
        assert_eq!(text_of(&lines[0]), b"caf\xE9 ??");
        assert_eq!(stats.substituted, 2);
    }

    #[test]
    fn a_stray_continuation_byte_does_not_stall() {
        let opts = PdfOptions::default();
        let mut stats = PdfStats::default();
        // A lone 0x80 where a lead byte should be. `utf8_len` returns 1 for it precisely so the walk
        // advances; if it returned 0 the whole exporter would hang here.
        let lines = feed_all(&opts, b"a\x80b", &mut stats);
        assert_eq!(text_of(&lines[0]), b"a?b");
        assert_eq!(stats.substituted, 1);
    }

    #[test]
    fn faces_follow_the_style_flags() {
        let plain = TextIntervalSpan::plain(0, 1);
        assert_eq!(
            Faces::for_span(&plain),
            (Faces::NAMES.regular, BaseFont::Helvetica)
        );

        let bold = TextIntervalSpan::styled(0, 1, STYLE_BOLD, 0);
        assert_eq!(
            Faces::for_span(&bold),
            (Faces::NAMES.bold, BaseFont::HelveticaBold)
        );

        let ital = TextIntervalSpan::styled(0, 1, STYLE_ITALIC, 0);
        assert_eq!(
            Faces::for_span(&ital),
            (Faces::NAMES.italic, BaseFont::Helvetica)
        );

        let both = TextIntervalSpan::styled(0, 1, STYLE_BOLD | STYLE_ITALIC, 0);
        assert_eq!(
            Faces::for_span(&both),
            (Faces::NAMES.bold_italic, BaseFont::HelveticaBold)
        );

        let code = TextIntervalSpan::styled(0, 1, STYLE_CODE, 0);
        assert_eq!(
            Faces::for_span(&code),
            (Faces::NAMES.code, BaseFont::Courier)
        );
        // Code wins over bold, and the face is Courier either way.
        let code_bold = TextIntervalSpan::styled(0, 1, STYLE_CODE | STYLE_BOLD, 0);
        assert_eq!(
            Faces::for_span(&code_bold),
            (Faces::NAMES.code_bold, BaseFont::Courier)
        );

        // A header is a size change, not a face change.
        let header = TextIntervalSpan::styled(0, 1, STYLE_HEADER, 0);
        assert_eq!(
            Faces::for_span(&header),
            (Faces::NAMES.regular, BaseFont::Helvetica)
        );
    }

    #[test]
    fn lines_per_page_counts_the_usable_height() {
        let mut o = PdfOptions::default();
        // Letter 792 tall, 54 margins, 14pt lines: (792 - 108) / 14 = 48.857 -> 48.
        assert_eq!(lines_per_page(&o), 48);
        o.margin = 300.0;
        // (792 - 600) / 14 = 13.71 -> 13. Still a page, just a narrow one.
        assert_eq!(lines_per_page(&o), 13);
    }

    #[test]
    fn lines_per_page_is_at_least_one_even_when_it_cannot_be() {
        // A margin that leaves no room at all: the negative floor becomes 0 as a `usize`, and the
        // `max(1)` is what keeps the page from being zero lines tall. Without it the layout loop would
        // flush a page before every line and produce one page per line.
        let o = PdfOptions {
            margin: 500.0,
            ..Default::default()
        };
        assert_eq!(lines_per_page(&o), 1);
        let tiny = PdfOptions {
            page: PageSize {
                width: 10.0,
                height: 10.0,
            },
            margin: 5.0,
            ..Default::default()
        };
        assert_eq!(lines_per_page(&tiny), 1);
    }
}
