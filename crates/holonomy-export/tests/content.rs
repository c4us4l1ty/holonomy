//! A minimal PDF content-stream reader, so the export gate can be a round trip.
//!
//! # Why not just grep the PDF for the source text
//!
//! Because it would pass on a broken PDF. Text in a PDF is a sequence of `(...)` operands wrapped in
//! `BT`/`Tf`/`Td`/`Tj`/`ET`, split wherever a line broke, and a line break is not a character in the
//! source -- there is no space or newline between "…end of line" and "start of next" unless the
//! exporter put one there. So "the source appears in the file" is not a property that means anything.
//! What means something is: *decoding the content stream and reassembling the lines puts the text back*.
//!
//! # What it handles, and what it does not
//!
//! Handles: `BT`/`ET`, `Tf`, `Td`, `TD`, `Tm`, `T*`, `Tj`, `TJ`, `'` and `"`, literal `(...)` strings
//! with backslash escapes, and `Tz` (horizontal scaling, which affects nothing here because it scales
//! an already-laid-out run). Numbers, names and arrays, so unknown operands can be stepped over without
//! desynchronising the tokeniser.
//!
//! Does not handle: compressed streams -- the exporter writes them uncompressed, and
//! [`PdfError`](holonomy_export::pdf::PdfError) has no variant for a filter, so a stream with one is a
//! bug in the exporter rather than a case to support. Form XObjects, Type3 fonts, and `TJ` kerning
//! numbers other than zero, none of which the exporter emits. **A change that makes the exporter emit
//! any of these needs this reader to change with it**, which is the right way for this to fail.
//!
//! # Line breaks are reconstructed from position, not from the source
//!
//! The reader sorts the text runs by their y coordinate and treats a change in y as a newline. That is
//! what a reader does, and it means a test failure here is a failure of the *layout*, not of a
//! bookkeeping convention: if the exporter emits two lines at the same y, they come back as one line
//! and the round trip fails.

use std::collections::BTreeMap;

/// Why a PDF could not be read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// No `%PDF-` header, so this is not a PDF.
    NoHeader,
    /// A stream's `stream` keyword was never followed by `endstream`.
    UnterminatedStream,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoHeader => write!(f, "no %PDF- header"),
            Self::UnterminatedStream => write!(f, "a stream is missing its endstream"),
        }
    }
}

/// One decoded string in a content stream, with the position it was drawn at.
#[derive(Debug, Clone, PartialEq)]
struct Run {
    /// Baseline, in points from the page bottom.
    y: f32,
    /// Left edge.
    x: f32,
    /// Decoded text.
    text: String,
}

/// The `/BaseFont` names a PDF declares, in the order they appear.
///
/// Counted by name rather than by substring: `/BaseFont /Helvetica` is a prefix of
/// `/BaseFont /Helvetica-Bold`, `/Helvetica-Oblique` and `/Helvetica-BoldOblique`, so a substring count
/// reports four for a one-entry request. Reading the name up to the line end is the thing that
/// distinguishes them.
pub fn base_fonts(pdf: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(pdf);
    let mut out = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.trim().strip_prefix("/BaseFont /") {
            out.push(rest.trim().to_string());
        }
    }
    out
}

/// Pull the uncompressed content streams out of a PDF, in object order.
pub fn content_streams(pdf: &[u8]) -> Result<Vec<(u32, Vec<u8>)>, ParseError> {
    if !pdf.starts_with(b"%PDF-") {
        return Err(ParseError::NoHeader);
    }
    let mut out = Vec::new();
    let mut rest = pdf;
    let mut object = 0u32;

    while let Some(at) = find(rest, b" obj") {
        let head = &rest[..at];
        // The object number is the last integer before ` obj`.
        let n = head
            .rsplit(|b| !b.is_ascii_digit())
            .find(|t| !t.is_empty())
            .and_then(|t| std::str::from_utf8(t).ok())
            .and_then(|t| t.parse::<u32>().ok())
            .unwrap_or(0);
        object = object.max(n);

        // The body is everything up to the next `obj`, `endobj`, or the trailer.
        let body_start = at + 4;
        let body_end = find_from(rest, body_start, b"endobj")
            .or_else(|| find(rest, b"trailer"))
            .unwrap_or(rest.len());
        let body = &rest[body_start..body_end.min(rest.len())];

        if let Some(s) = find(body, b"stream") {
            let mut data_start = s + b"stream".len();
            // A stream keyword is followed by CRLF or LF, per the spec.
            if body.get(data_start) == Some(&b'\r') {
                data_start += 1;
            }
            if body.get(data_start) == Some(&b'\n') {
                data_start += 1;
            }
            let data_end = find(body, b"endstream").ok_or(ParseError::UnterminatedStream)?;
            // `endstream` is preceded by an EOL that is not part of the data.
            let mut end = data_end;
            if body.get(end.wrapping_sub(1)) == Some(&b'\n') {
                end -= 1;
            }
            if body.get(end.wrapping_sub(1)) == Some(&b'\r') {
                end -= 1;
            }
            out.push((n, body[data_start..end.max(data_start)].to_vec()));
        }

        rest = &rest[body_end.min(rest.len())..];
        if rest.len() < 8 {
            break;
        }
    }
    Ok(out)
}

/// Decode a PDF file's text, reassembled in reading order.
///
/// This is the function the gate's round trip rests on. See the module docs for what it does and does
/// not handle.
pub fn extract_text(pdf: &[u8]) -> Result<String, ParseError> {
    let streams = content_streams(pdf)?;
    // **Per stream, not per file.** Each content stream is one page, and pages reuse the same y
    // coordinates -- page 1's last line and page 2's first can both be at y=700. Assembling across
    // streams therefore merged two lines from different pages into one, in x order, which produced
    // text that was neither page's. The fix is to assemble each page on its own and join, which is
    // also what a reader does.
    let mut pages: Vec<String> = Vec::new();
    for (_, data) in streams {
        let mut runs = decode_stream(&data);
        let page = assemble(&mut runs);
        if !page.is_empty() {
            pages.push(page);
        }
    }
    Ok(pages.join("\n"))
}

/// Decode one content stream into positioned runs.
fn decode_stream(data: &[u8]) -> Vec<Run> {
    let mut runs = Vec::new();
    let mut toks = Tokenizer { data, at: 0 };

    // The text-space line matrix, per PDF 9.4.1.
    //
    // Two positions, not one, because `Td` is relative to the **start of the current line**, not to
    // the current point -- and the difference is invisible for an exporter that opens a fresh `BT`
    // per line (which this one does, so every line's `Td` is effectively absolute) while being very
    // visible for one that opens a single `BT` for the page. Tracking only the current point made
    // `td_moves_in_the_direction_the_operator_says` produce (108, 1462) for a second `54 724 Td`
    // where PDF says (108, 1462) too -- the model was accidentally right only because of the
    // exporter's shape.
    let (mut line_x, mut line_y) = (0f32, 0f32);
    let (mut x, mut y) = (0f32, 0f32);
    let mut leading = 0f32;
    // Operands accumulate until a keyword consumes them.
    let mut operands: Vec<Operand> = Vec::new();
    // The array currently being read, for `TJ`. Kept apart from `operands` so a `TJ` operand list is
    // not confused with the array it wraps.
    let mut array: Option<Vec<Operand>> = None;

    while let Some(tok) = toks.next_token() {
        match tok {
            // A name is an operand of `Tf` and of nothing else this reader models, and the exporter
            // never puts one between two operands of the same operator. Skipping it outright is
            // therefore right: pushing it would leave a `Name` sitting in the operand list where a
            // number was expected, and `num` skips non-numbers so it would work -- but only by accident.
            Token::Name(_) => {}
            Token::ArrayStart => array = Some(Vec::new()),
            Token::ArrayEnd => {
                if let Some(a) = array.take() {
                    operands.push(Operand::Array(a));
                }
            }
            Token::Number(n) => push(Operand::Num(n), &mut array, &mut operands),
            Token::Str(s) => push(Operand::Str(s), &mut array, &mut operands),
            Token::Keyword(k) => {
                match k.as_slice() {
                    b"BT" => {
                        // `BT` resets the line matrix to the identity.
                        line_x = 0.0;
                        line_y = 0.0;
                        x = 0.0;
                        y = 0.0;
                    }
                    b"ET" => {}
                    // `tx ty Td`. **The x translation comes first.** Reading these the other way
                    // round -- which is easy, because counting back from the end of the operand list
                    // gives `ty` first -- makes every line's y the left margin, so every line lands at
                    // the same height and the whole page reassembles as one line. That is exactly what
                    // `pdf_wrapping_preserves_reading_order` caught.
                    b"Td" | b"TD" => {
                        // The x translation comes first. Counting back from the end of the operand
                        // list gives `ty` first, and taking it as `tx` makes every line's y the left
                        // margin -- which put the whole page on one baseline and merged it into one
                        // line. `pdf_wrapping_preserves_reading_order` is what caught that.
                        let (tx, ty) = (num(&operands, 1), num(&operands, 0));
                        if let (Some(tx), Some(ty)) = (tx, ty) {
                            line_x += tx;
                            line_y += ty;
                            x = line_x;
                            y = line_y;
                            if k == b"TD" {
                                // `TD` is `Td` plus `TL -ty`. Implemented because an exporter using
                                // `TD` for its moves would otherwise have every `T*` land wrong.
                                leading = -ty;
                            }
                        }
                    }
                    b"TL" => leading = num(&operands, 0).unwrap_or(leading),
                    // `a b c d e f Tm`, with `e` the x translation and `f` the y. Counted back from
                    // the end for the same robustness as `Td`: six positions assume nothing else
                    // leaked into the operand list, two do not.
                    b"Tm" => {
                        // `a b c d e f Tm`; `e` and `f` are the translation. Counted back from the end
                        // rather than at fixed positions so a stray operand cannot shift them.
                        if let (Some(tx), Some(ty)) = (num(&operands, 1), num(&operands, 0)) {
                            line_x = tx;
                            line_y = ty;
                            x = tx;
                            y = ty;
                        }
                    }
                    b"T*" => {
                        line_y -= leading;
                        x = line_x;
                        y = line_y;
                    }
                    b"Tj" => {
                        if let Some(Operand::Str(bytes)) = operands.last() {
                            runs.push(Run {
                                y,
                                x,
                                text: decode_string(bytes),
                            });
                        }
                    }
                    b"'" | b"\"" => {
                        // `'` and `"` move to the next line *before* showing.
                        line_y -= leading;
                        x = line_x;
                        y = line_y;
                        if let Some(Operand::Str(bytes)) = operands.last() {
                            runs.push(Run {
                                y,
                                x,
                                text: decode_string(bytes),
                            });
                        }
                    }
                    b"TJ" => {
                        if let Some(Operand::Array(items)) = operands.last() {
                            let mut text = String::new();
                            for item in items {
                                if let Operand::Str(bytes) = item {
                                    text.push_str(&decode_string(bytes));
                                }
                            }
                            runs.push(Run { y, x, text });
                        }
                    }
                    // Every other operator -- path construction, graphics state, the font and size
                    // selection we deliberately ignore -- still clears the operand list. Not doing so
                    // is how a reader ends up feeding `1 2` to `Tj`.
                    _ => {}
                }
                operands.clear();
            }
        }
        // Bound the list so a malformed stream cannot grow it without limit.
        if operands.len() > 64 {
            operands.drain(..32);
        }
    }
    runs
}

/// Add one operand to the open array, or to the top-level list if no array is open.
fn push(op: Operand, array: &mut Option<Vec<Operand>>, operands: &mut Vec<Operand>) {
    match array {
        Some(a) => a.push(op),
        None => operands.push(op),
    }
}

/// A PDF operand.
#[derive(Debug, Clone, PartialEq)]
enum Operand {
    /// A number. `f32` because the PDF syntax has reals; the *layout* decisions are made with integers
    /// in the exporter, so nothing here rounds a line break.
    Num(f32),
    /// A literal or hex string, escapes not yet resolved.
    Str(Vec<u8>),
    /// A `[...]` array.
    Array(Vec<Operand>),
}

/// The `n`-th numeric operand, counting **back from the end**.
///
/// From the end rather than from the start because operators that take their last operand in a
/// meaningful position -- `TL f`, `T*` with none, `Tf /F size` -- are the common case, and a
/// fixed-count-from-the-front would be wrong the moment an operand leaked in from an operator this
/// reader does not model.
fn num(operands: &[Operand], from_end: usize) -> Option<f32> {
    operands
        .iter()
        .rev()
        .filter(|o| matches!(o, Operand::Num(_)))
        .nth(from_end)
        .and_then(|o| match o {
            Operand::Num(n) => Some(*n),
            _ => None,
        })
}

/// A PDF literal string's escapes, resolved.
fn decode_string(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            out.push(bytes[i] as char);
            i += 1;
            continue;
        }
        i += 1;
        match bytes.get(i) {
            Some(b'n') => {
                out.push('\n');
                i += 1;
            }
            Some(b'r') => {
                out.push('\r');
                i += 1;
            }
            Some(b't') => {
                out.push('\t');
                i += 1;
            }
            Some(b'b') | Some(b'f') => {
                // Backspace and form feed. Rare, but a raw byte would be worse.
                out.push('\u{8}');
                i += 1;
            }
            Some(b'\n') => i += 1, // line continuation
            Some(o) => {
                out.push(*o as char);
                i += 1;
            }
            None => {}
        }
    }
    out
}

/// Reassemble positioned runs into text, in reading order.
///
/// Runs are bucketed by y and each bucket is joined, because two runs on one line are one line.
///
/// **Buckets come out in *descending* y.** A PDF page has its origin at the bottom left, so `y` grows
/// upward and the topmost line has the largest value. Sorting ascending would emit the document
/// backwards -- which is exactly what this function did until
/// `a_line_of_runs_at_one_y_joins_into_one_line` caught it. Getting it right is what makes the export
/// gate's round trip a test of the *layout* rather than of the reader.
fn assemble(runs: &mut [Run]) -> String {
    if runs.is_empty() {
        return String::new();
    }
    // Round y to a tenth of a point: the exporter writes one decimal, but a reader should not depend
    // on the exact representation.
    let mut by_line: BTreeMap<i64, Vec<(f32, String)>> = BTreeMap::new();
    for r in runs.iter() {
        // **Empty runs are kept.** The exporter writes a blank line as a positioned empty `Tj`, and
        // skipping empty runs here threw away every paragraph break -- so a document with a blank line
        // between its paragraphs extracted as one paragraph. A line's existence is its *position*, not
        // its text.
        by_line
            .entry((r.y * 10.0).round() as i64)
            .or_default()
            .push((r.x, r.text.clone()));
    }
    let mut out = String::new();
    let mut keys: Vec<i64> = by_line.keys().rev().copied().collect();
    keys.dedup();
    for key in keys {
        let mut items = by_line[&key].clone();
        // Within a line, left to right.
        items.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        for (_, t) in items {
            out.push_str(&t);
        }
        out.push('\n');
    }
    // Trailing newline is an artefact of the assembly, not of the document.
    if out.ends_with('\n') {
        out.pop();
    }
    out
}

// ------------------------------------------------------------------ tokenizer

/// A PDF token.
#[derive(Debug, Clone, PartialEq)]
enum Token {
    Number(f32),
    Str(Vec<u8>),
    /// A `/Name`, with the leading `/` stripped.
    ///
    /// Distinct from [`Keyword`](Self::Keyword) because the two cannot be told apart by their bytes:
    /// `'` and `"` are *operators* that begin with punctuation, so "operators start with a letter" is
    /// false and a guard on that would swallow them. The tokenizer knows which it saw, so it says so.
    Name(Vec<u8>),
    ArrayStart,
    ArrayEnd,
    /// An operator: `BT`, `Tf`, `Tj`, `'`, `"` and so on.
    Keyword(Vec<u8>),
}

/// Reads PDF tokens, aware of `(...)` strings, `<...>` hex strings and `/Name`s.
struct Tokenizer<'a> {
    data: &'a [u8],
    at: usize,
}

impl Tokenizer<'_> {
    /// Skip whitespace and comments. Returns the next significant byte.
    fn skip_filler(&mut self) -> Option<u8> {
        while self.at < self.data.len() {
            let b = self.data[self.at];
            match b {
                b' ' | b'\t' | b'\r' | b'\n' | 0x0c => self.at += 1,
                b'%' => {
                    // A comment runs to end of line.
                    while self.at < self.data.len()
                        && self.data[self.at] != b'\n'
                        && self.data[self.at] != b'\r'
                    {
                        self.at += 1;
                    }
                }
                _ => return Some(b),
            }
        }
        None
    }

    fn next_token(&mut self) -> Option<Token> {
        let b = self.skip_filler()?;
        match b {
            b'(' => {
                self.at += 1;
                let mut depth = 1i32;
                let mut out = Vec::new();
                while self.at < self.data.len() {
                    let c = self.data[self.at];
                    match c {
                        b'\\' => {
                            // Keep the escape and the next byte; `decode_string` resolves them.
                            out.push(c);
                            self.at += 1;
                            if let Some(n) = self.data.get(self.at) {
                                out.push(*n);
                                self.at += 1;
                            }
                        }
                        b'(' => {
                            depth += 1;
                            out.push(c);
                            self.at += 1;
                        }
                        b')' => {
                            depth -= 1;
                            self.at += 1;
                            if depth == 0 {
                                break;
                            }
                            out.push(c);
                        }
                        _ => {
                            out.push(c);
                            self.at += 1;
                        }
                    }
                }
                Some(Token::Str(out))
            }
            b'<' if self.data.get(self.at + 1) == Some(&b'<') => {
                self.at += 2;
                Some(Token::ArrayStart)
            }
            b'>' if self.data.get(self.at + 1) == Some(&b'>') => {
                self.at += 2;
                Some(Token::ArrayEnd)
            }
            b'<' => {
                // Hex string.
                self.at += 1;
                let mut out = Vec::new();
                let mut hi: Option<u8> = None;
                while self.at < self.data.len() && self.data[self.at] != b'>' {
                    let c = self.data[self.at];
                    self.at += 1;
                    let Some(v) = (c as char).to_digit(16) else {
                        continue;
                    };
                    match hi {
                        None => hi = Some(v as u8),
                        Some(h) => {
                            out.push(h << 4 | v as u8);
                            hi = None;
                        }
                    }
                }
                self.at += 1;
                Some(Token::Str(out))
            }
            b'/' => {
                self.at += 1;
                let start = self.at;
                while self.at < self.data.len() && !is_delimiter(self.data[self.at]) {
                    self.at += 1;
                }
                let mut n = self.data[start..self.at].to_vec();
                if n.first() == Some(&b'#') {
                    // A `#xx` escape inside a name.
                    n = decode_name_hash(&n);
                }
                Some(Token::Name(n))
            }
            b'[' => {
                self.at += 1;
                Some(Token::ArrayStart)
            }
            b']' => {
                self.at += 1;
                Some(Token::ArrayEnd)
            }
            _ => {
                let start = self.at;
                while self.at < self.data.len() && !is_delimiter(self.data[self.at]) {
                    self.at += 1;
                }
                let word = &self.data[start..self.at];
                if word.is_empty() {
                    self.at += 1;
                    return self.next_token();
                }
                match std::str::from_utf8(word)
                    .ok()
                    .and_then(|s| s.parse::<f32>().ok())
                {
                    Some(n) => Some(Token::Number(n)),
                    None => Some(Token::Keyword(word.to_vec())),
                }
            }
        }
    }
}

/// Resolve `#xx` escapes in a name.
fn decode_name_hash(name: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(name.len());
    let mut i = 0usize;
    while i < name.len() {
        if name[i] == b'#' && i + 2 < name.len() {
            let hex = std::str::from_utf8(&name[i + 1..i + 3]).ok();
            if let Some(v) = hex.and_then(|s| u8::from_str_radix(s, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(name[i]);
        i += 1;
    }
    out
}

/// Whether `b` terminates a regular token or a name.
///
/// Whitespace is in this set, and that is worth spelling out: PDF's own definition of a *delimiter*
/// is `()<>[]{}/%` and excludes whitespace, but a regular token still ends at whitespace. Leaving it
/// out made `BT ET` lex as a single keyword named `BT ET`, which then matched no operator -- so the
/// reader silently saw an empty page.
///
/// The set is a superset of PDF's delimiters on purpose: the extra characters (`{`, `}`) are not
/// delimiters, and treating them as such only means an unbalanced brace ends a token early. That is
/// strictly safer than the alternative, because the exporter emits no braces at all.
#[inline]
fn is_delimiter(b: u8) -> bool {
    matches!(
        b,
        b' ' | b'\t'
            | b'\r'
            | b'\n'
            | 0x0c
            | b'('
            | b')'
            | b'<'
            | b'>'
            | b'['
            | b']'
            | b'{'
            | b'}'
            | b'/'
            | b'%'
    )
}

#[inline]
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    find_from(haystack, 0, needle)
}

#[inline]
fn find_from(haystack: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || from > haystack.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|i| i + from)
}

// ------------------------------------------------------------------ text helpers

/// The literal text in `html`, with every tag removed.
pub fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// Resolve HTML entities. The inverse of the exporter's escaping.
pub fn unescape(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let b = html.as_bytes();
    let mut i = 0usize;
    while i < b.len() {
        if b[i] != b'&' {
            out.push(b[i] as char);
            i += 1;
            continue;
        }
        let rest = &html[i..];
        let found = [
            ("&lt;", '<'),
            ("&gt;", '>'),
            ("&quot;", '"'),
            ("&#39;", '\''),
            ("&amp;", '&'),
        ]
        .into_iter()
        .find(|(seq, _)| rest.starts_with(seq));
        match found {
            Some((seq, c)) => {
                out.push(c);
                i += seq.len();
            }
            None => {
                out.push('&');
                i += 1;
            }
        }
    }
    out
}

/// How many times `needle` occurs in `haystack`.
pub fn count(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    haystack.matches(needle).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_unescape() {
        assert_eq!(decode_string(br"a\(b"), "a(b");
        assert_eq!(decode_string(br"a\\b"), "a\\b");
        assert_eq!(decode_string(b"a\\nb"), "a\nb");
        assert_eq!(decode_string(b"a\\rb"), "a\rb");
        assert_eq!(decode_string(b"a\\tb"), "a\tb");
    }

    #[test]
    fn nested_parentheses_in_a_string_survive() {
        // PDF balances parentheses inside a string, so a naive reader that stops at the first `)`
        // truncates. This is the exporter's own escaping, read back.
        assert_eq!(decode_string(br"f(1)"), "f(1)");
        assert_eq!(decode_string(br"\(a(b\)"), "(a(b)");
    }

    #[test]
    fn backslashes_are_the_case_that_matters() {
        // A document containing a backslash is the one that breaks a naive PDF writer, because `\` is
        // the escape character inside a literal string.
        assert_eq!(decode_string(br"C:\\path\\to"), "C:\\path\\to");
    }

    #[test]
    fn tokenizing_finds_operators_and_numbers() {
        let mut t = Tokenizer {
            data: b"BT /F1 11 Tf 1 2 Td (hi) Tj ET",
            at: 0,
        };
        let got: Vec<Token> = std::iter::from_fn(|| t.next_token()).collect();
        assert_eq!(
            got,
            vec![
                Token::Keyword(b"BT".to_vec()),
                Token::Name(b"F1".to_vec()),
                Token::Number(11.0),
                Token::Keyword(b"Tf".to_vec()),
                Token::Number(1.0),
                Token::Number(2.0),
                Token::Keyword(b"Td".to_vec()),
                Token::Str(b"hi".to_vec()),
                Token::Keyword(b"Tj".to_vec()),
                Token::Keyword(b"ET".to_vec()),
            ]
        );
    }

    #[test]
    fn comments_are_skipped() {
        let mut t = Tokenizer {
            data: b"% a comment\nBT ET",
            at: 0,
        };
        let got: Vec<Token> = std::iter::from_fn(|| t.next_token()).collect();
        assert_eq!(
            got,
            vec![
                Token::Keyword(b"BT".to_vec()),
                Token::Keyword(b"ET".to_vec())
            ]
        );
    }

    #[test]
    fn a_line_of_runs_at_one_y_joins_into_one_line() {
        let mut runs = vec![
            Run {
                y: 700.0,
                x: 50.0,
                text: "world".into(),
            },
            Run {
                y: 700.0,
                x: 10.0,
                text: "hello ".into(),
            },
            Run {
                y: 690.0,
                x: 10.0,
                text: "second".into(),
            },
        ];
        assert_eq!(assemble(&mut runs), "hello world\nsecond");
    }

    #[test]
    fn td_moves_in_the_direction_the_operator_says() {
        // `tx ty Td`: the x translation comes *first*. Reading the operands the other way round makes
        // every line's y the left margin, so every line lands at the same height and the page
        // reassembles as one line.
        //
        // Asserted on the resulting positions, because positions are the thing that has to be right.
        // Each line gets its own `BT`, which is what the exporter emits and what makes its `Td`s
        // absolute in practice -- `td_is_relative_to_the_line_start` covers the other case.
        let mut runs = decode_stream(
            b"BT /F1 11 Tf 54 738 Td (first) Tj ET BT /F1 11 Tf 54 724 Td (second) Tj ET",
        );
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].text, "first");
        assert_eq!((runs[0].x, runs[0].y), (54.0, 738.0));
        assert_eq!(runs[1].text, "second");
        assert_eq!((runs[1].x, runs[1].y), (54.0, 724.0));
        assert!(runs[0].y > runs[1].y, "top line must have the larger y");
        assert_eq!(assemble(&mut runs), "first\nsecond");
    }

    #[test]
    fn td_is_relative_to_the_line_start_not_the_point() {
        // PDF 9.4.1: `Td` translates the *line matrix*. So two `Td`s inside one `BT` accumulate, and a
        // reader that treated them as absolute would place both lines at the margin.
        let mut runs =
            decode_stream(b"BT /F1 11 Tf 54 738 Td (a) Tj 0 -14 Td (b) Tj 0 -14 Td (c) Tj ET");
        assert_eq!(runs.len(), 3);
        assert_eq!((runs[0].x, runs[0].y), (54.0, 738.0));
        assert_eq!((runs[1].x, runs[1].y), (54.0, 724.0));
        assert_eq!((runs[2].x, runs[2].y), (54.0, 710.0));
        assert_eq!(assemble(&mut runs), "a\nb\nc");
    }

    #[test]
    fn a_name_between_two_operands_does_not_discard_them() {
        // A `/Name` is lexed as a keyword, so without the alpha guard it would hit the catch-all arm and
        // clear the operand list -- which for `Tm` would leave the position unset and the run placed at
        // the origin.
        let runs = decode_stream(b"BT /F1 11 Tf 0 0 0 0 /X 100 500 Tm (a) Tj ET");
        assert_eq!(runs.len(), 1);
        assert_eq!((runs[0].x, runs[0].y), (100.0, 500.0));
    }

    #[test]
    fn bt_resets_the_line_matrix() {
        // A second `BT` starts from the identity again, so the same `Td` lands in the same place.
        let runs =
            decode_stream(b"BT /F1 11 Tf 10 100 Td (a) Tj ET BT /F1 11 Tf 10 100 Td (b) Tj ET");
        assert_eq!(runs[0].y, 100.0);
        assert_eq!(runs[1].y, 100.0);
    }

    #[test]
    fn tm_sets_the_position_outright() {
        let mut runs =
            decode_stream(b"BT /F1 11 Tf 0 0 0 0 100 500 Tm (a) Tj 0 0 0 0 100 400 Tm (b) Tj ET");
        assert_eq!(runs.len(), 2);
        assert_eq!((runs[0].x, runs[0].y), (100.0, 500.0));
        assert_eq!((runs[1].x, runs[1].y), (100.0, 400.0));
        assert_eq!(assemble(&mut runs), "a\nb");
    }

    #[test]
    fn td_is_td_which_also_sets_the_leading() {
        // `TD tx ty` is `Td` plus `TL -ty`, so `T*` then moves down by the leading.
        let runs = decode_stream(b"BT /F1 11 Tf 0 100 TD (a) Tj T* (b) Tj ET");
        assert_eq!(runs[0].y, 100.0);
        // `TD 0 100` sets the leading to -100, so `T*` subtracts a negative and the line moves *up*.
        // Counter-intuitive, and correct: a positive `ty` on `TD` is not how one writes "next line".
        assert_eq!(runs[1].y, 200.0);

        // The other direction, which is the one that means "next line": a negative ty.
        let mut down = decode_stream(b"BT /F1 11 Tf 0 -14 TD (a) Tj T* (b) Tj ET");
        assert_eq!(down[0].y, -14.0);
        assert_eq!(down[1].y, -28.0, "T* moved down by the leading");
        assert_eq!(assemble(&mut down), "a\nb");
    }

    #[test]
    fn tl_alone_sets_the_leading() {
        let runs = decode_stream(b"BT /F1 11 Tf 14 TL 0 700 Td (a) Tj T* (b) Tj ET");
        assert_eq!(runs[0].y, 700.0);
        assert_eq!(runs[1].y, 686.0);
    }

    #[test]
    fn quotes_move_to_the_next_line_before_showing() {
        // `'` is "next line, then show text".
        let runs = decode_stream(b"BT /F1 11 Tf 14 TL 0 700 Td (a) Tj (b) ' ET");
        assert_eq!(runs[1].y, 686.0);
        assert_eq!(runs[1].text, "b");
    }

    #[test]
    fn two_pages_at_the_same_height_do_not_merge() {
        // The bug this guards: pages reuse y coordinates, so assembling across streams merged page 1's
        // line with page 2's and the text came out as neither page's.
        let page1 = b"BT /F1 11 Tf 50 700 Td (page one) Tj ET";
        let page2 = b"BT /F1 11 Tf 50 700 Td (page two) Tj ET";
        let mut one = decode_stream(page1);
        let mut two = decode_stream(page2);
        assert_eq!(assemble(&mut one), "page one");
        assert_eq!(assemble(&mut two), "page two");
        // Assembled together they would collide on y=700, which is why `extract_text` does not.
        let mut both: Vec<Run> = one.iter().chain(two.iter()).cloned().collect();
        assert_eq!(assemble(&mut both), "page onepage two");
    }

    #[test]
    fn base_fonts_are_read_by_name_not_by_substring() {
        let pdf = b"\n  /BaseFont /Helvetica\n  /BaseFont /Helvetica-Bold\n  /BaseFont /Courier\n";
        // A name that contains whitespace would be written `/Name#20x`, so the line end is a safe
        // terminator here.
        assert_eq!(
            base_fonts(pdf),
            vec!["Helvetica", "Helvetica-Bold", "Courier"]
        );
    }

    #[test]
    fn an_empty_stream_gives_empty_text() {
        assert_eq!(assemble(&mut []), "");
        let mut runs = vec![Run {
            y: 1.0,
            x: 1.0,
            text: String::new(),
        }];
        assert_eq!(assemble(&mut runs), "");
    }

    #[test]
    fn html_helpers_round_trip() {
        let src = "a < b & c > d \"e\" 'f'";
        let mut esc = String::new();
        for b in src.bytes() {
            holonomy_export::html::push_escaped_byte(unsafe { esc.as_mut_vec() }, b);
        }
        assert_eq!(unescape(&strip_tags(&esc)), src);
    }

    #[test]
    fn a_bare_ampersand_is_not_consumed() {
        assert_eq!(unescape("R&D &unknown; &"), "R&D &unknown; &");
    }
}
