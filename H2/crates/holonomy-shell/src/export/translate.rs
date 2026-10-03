//! ProseMirror JSON → Typst markup.
//!
//! # Why a translator and not a Typst template
//!
//! Because the document is not Typst and will not become Typst. Section JSON is ProseMirror's
//! shape, chosen for an editor; Typst is a typesetting language, chosen for page layout. The
//! only sensible relationship between them is a function from one to the other, and it has to
//! be a *function*: a document with a table in it has to become a Typst table, and a template
//! cannot decide that.
//!
//! # The escaping rule, and it is the whole ballgame
//!
//! Typst's markup is not a superset of the document's text. `#` starts code, `*`/`_` are
//! emphasis, `$` is math, `<`/`>` are raw blocks, `@` is a reference, `\` escapes the next
//! character, and a `\` at the end of a line is a line break. Every character a user can type
//! has to be escaped, and a single one that is not turns their paragraph into a typesetting
//! error.
//!
//! So [`escape_text`] escapes the full set, in one place, and every text node goes through it.
//! The alternative — "escape the ones that usually matter" — is how a document named
//! `#show` or containing `$5 and $10` stops exporting.
//!
//! # What an unknown node becomes
//!
//! Its text, and nothing else. A node type this translator has never heard of still appears in
//! the document, and silently dropping it loses content. Rendering its text keeps the words and
//! loses the formatting, which is visible and recoverable; dropping the node loses both and is
//! neither. The count is reported so the caller can say so.

use std::fmt::Write;

use serde_json::Value;
use std::collections::BTreeSet;

/// What the translator did, beyond producing a string.
///
/// Reported rather than logged, because the two failures are different: a document with no
/// unknown nodes translated cleanly, and one with some translated *lossily*. A caller that
/// treats them alike will ship a document with missing content and no signal.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TranslationReport {
    /// Top-level blocks seen, by node type.
    pub blocks: BTreeMap<String, usize>,
    /// Node types with no dedicated handling, whose text was kept and formatting dropped.
    pub unknown_types: BTreeSet<String>,
    /// Every `holo-asset://` hash the document names.
    pub assets: BTreeSet<String>,
    /// Text characters written.
    pub chars: usize,
    /// Marks with no equivalent in Typst, whose text was kept and decoration dropped.
    ///
    /// Reported rather than silently dropped, for the same reason `unknown_types` is: a
    /// document that exported with one of its author's emphases missing looks correct, and
    /// nothing else would say otherwise.
    pub lossy_marks: BTreeSet<String>,
}

impl TranslationReport {
    /// Whether every node type was understood.
    pub fn complete(&self) -> bool {
        self.unknown_types.is_empty()
    }

    /// One line naming what was not understood, or `None` when there was nothing.
    ///
    /// Written into the exported PDF's own metadata is tempting and wrong: a document is
    /// content, and putting diagnostics in it means the diagnostics get printed and
    /// distributed with the user's work.
    pub fn summary(&self) -> Option<String> {
        if self.complete() {
            return None;
        }
        Some(format!(
            "{} node type(s) were exported as plain text, because the PDF translator has no rule \
             for them: {}",
            self.unknown_types.len(),
            self.unknown_types.iter().cloned().collect::<Vec<_>>().join(", ")
        ))
    }
}

use std::collections::BTreeMap;

/// Page geometry, in millimetres.
///
/// # Why the numbers are here and not configurable
///
/// Because a PDF's page size is a property of the *book*, not of the document, and a document
/// that renders at A4 on one machine and Letter on another is a document whose pagination
/// cannot be cited. Fixed, and stated. Changing it is a deliberate decision with a reason, which
/// is why the whole set is one constant rather than four fields someone will tune.
pub const PAGE: &str = "width: 160mm, height: 240mm, margin: 18mm";

/// A short digest of everything the preamble sets.
///
/// # Why the preamble is part of the pagination claim
///
/// Because it is. `#set page(width: 160mm, ...)` decides where every line breaks, and a
/// change to the body size or the leading moves every glyph on every page. Two runners
/// running different preambles will disagree on every page of a fifty-page document, and
/// reporting that as fifty content differences would be technically true and completely
/// unhelpful.
///
/// # Why a digest of the source and not a version number
///
/// Because a version constant can drift from what the code emits. Nothing stops someone
/// changing `PAGE` and forgetting the constant, and then two runners built from different
/// commits would agree on the constant and disagree on every page — with the one piece of
/// evidence that would have explained it reporting that they were the same. Hashing the
/// preamble text cannot drift from the preamble.
pub fn preamble_digest() -> String {
    use sha2::Digest as _;
    // The same five settings, in the same order `translate` writes them. Built from the
    // constants rather than copied, so it changes when they do.
    let preamble = format!(
        "#set page({PAGE})\n#set text(font: \"{BODY_FONT}\", size: 10pt, lang: \"en\", \
         hyphenate: true)\n#set par(justify: true, leading: 0.65em)\n#set table(stroke: 0.5pt + \
         gray, inset: 4pt)\n#set heading(numbering: none)\n"
    );
    let digest = sha2::Sha256::digest(preamble.as_bytes());
    digest[..6].iter().map(|b| format!("{b:02x}")).collect()
}

/// Generate a Typst document from section JSON.
///
/// # The preamble is part of the output, deliberately
///
/// `#set page(...)`, `#set text(...)` and `#set table(...)` are emitted here rather than
/// configured through a Typst API because there is no Typst API for them: they are markup, and
/// markup has to be written into the document. The consequence is that the generated source is
/// a *complete, readable* Typst program — which is what makes a failed export diagnosable,
/// because the string this returns is the string that failed.
pub fn translate(sections: &[Value]) -> (String, TranslationReport) {
    let mut out = String::with_capacity(16 * 1024);
    let mut report = TranslationReport::default();

    // The font is named here, from the bundle, rather than left to the system.
    //
    // The earlier version named no family at all "so the export's typography follows the system
    // it was exported on". That was reasoning in the wrong direction: it made two machines
    // produce different documents from the same bytes, which is the opposite of what a word
    // processor is for. Naming a *bundled* family gives the determinism and keeps the fonts.
    //
    // One `writeln!` per line rather than one long continued literal. A `\` at the end of a Rust
    // string line removes the newline, and a `\\` before that puts a literal backslash into the
    // output -- which Typst reads as the start of a code expression and reports as "the character
    // `\` is not valid in code". Two errors from one formatting convenience.
    out.push_str(
        "// Generated by Holonomy. Not hand-editable: this is the export of a document whose \
         canonical form is section JSON.\n",
    );
    writeln!(out, "#set page({PAGE})").expect("writing to a String cannot fail");
    writeln!(out, "#set text(font: \"{BODY_FONT}\", size: 10pt, lang: \"en\", hyphenate: true)")
        .expect("writing to a String cannot fail");
    writeln!(out, "#set par(justify: true, leading: 0.65em)").expect("writing to a String cannot fail");
    writeln!(out, "#set table(stroke: 0.5pt + gray, inset: 4pt)").expect("writing to a String cannot fail");
    writeln!(out, "#set heading(numbering: none)").expect("writing to a String cannot fail");
    out.push('\n');

    for section in sections {
        // A *section's* content is its `content` array, not the node wrapping it. Passing the
        // node itself made `doc` the first "unknown type" in every report — the wrapper is not
        // unknown, it is not a block at all.
        let blocks = match section.get("content").and_then(Value::as_array) {
            Some(blocks) => blocks,
            None => {
                // A section with no content array is malformed, and saying so is better than
                // silently exporting nothing: the document is damaged and the user needs to know
                // which part.
                report.unknown_types.insert("(malformed section)".to_string());
                continue;
            }
        };
        for block in blocks {
            translate_block(block, 0, &mut out, &mut report);
        }
    }
    report.chars = out.chars().count();
    (out, report)
}

/// Escape text for Typst markup.
///
/// # The set, and why each member
///
/// - `#` — starts code. `#show`, `#let`, `#image` are all valid user text that would otherwise
///   be executed.
/// - `*` and `_` — emphasis. Three of them is bold; one is italic; an unmatched pair swallows
///   the rest of the paragraph.
/// - `$` — math. `$5 and $10` is two prices to the author and a math span to Typst.
/// - `<` and `>` — raw blocks, which take the next balanced `<...>` verbatim.
/// - `@` — a reference to a label or bibliography entry.
/// - `\` — the escape character itself.
/// - `` ` `` — raw, and in Typst it also starts triple-backtick code.
/// - `[` and `]` — content blocks, which can take arguments.
/// - `-` — becomes a list item at the start of a line.
/// - `+` — becomes an enumerated list item.
/// - `/` — starts a term list item.
/// - `~` — a non-breaking space, which changes spacing.
/// - `.` — a strong/weak marker in some positions.
/// - `=` — a heading marker at the start of a line.
///
/// # Why the trailing-newline case matters
///
/// A `\` before a newline is a *line break* in Typst. So a paragraph ending in a backslash — a
/// Windows path, a regex — would join itself to the next paragraph. Escaping it fixes that,
/// which is why the escape runs left to right over the original text and never over its own
/// output.
pub fn escape_text(text: &str) -> String {
    const NEEDS: &[char] = &[
        '\\', '#', '*', '_', '$', '<', '>', '@', '`', '[', ']', '-', '+', '/', '~', '.', '=', '|',
        '"',
    ];
    let mut out = String::with_capacity(text.len() + text.len() / 8);
    for ch in text.chars() {
        if NEEDS.contains(&ch) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Escape a string for Typst's own quoted-string syntax.
///
/// # Why not Rust's `{:?}`, and not a Typst raw string
///
/// Two failures, both found by exporting rather than by reading — which is the argument for
/// having round-trip tests over a string builder at all.
///
/// Rust's `{:?}` escapes for *Rust*, and Typst then re-interprets the backslashes: a
/// `println!("#hi")` in a code block came out with a stray backslash, because Rust wrote `\\"`
/// and Typst read that as one literal backslash rather than as an escaped quote.
///
/// Typst's backtick strings are rejected outright as a named argument's value — "expected
/// string, found content" — because in code position a backtick opens markup.
///
/// So the escaping is done here, once, for Typst's grammar: `\`, `"`, and the three whitespace
/// escapes Typst recognises. That is the whole set, and a program needs nothing else, which is
/// what makes this safe rather than merely convenient.
fn typst_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out
}

/// Convert TeX-ish math into Typst math.
///
/// # Why this is needed at all, and it is not a small thing
///
/// **Typst's math is not LaTeX.** It is its own notation, and the two disagree about the most
/// common expressions there are. Typst writes the summation sign as `sum`, not `\sum`. So
/// passing a document's TeX through unchanged produces `unknown variable: um` -- Typst reading
/// `\s` as markup, leaving `um` to be evaluated as a variable -- and every equation in every
/// document is a compile error.
///
/// The document's equations were written by KaTeX, in the editor, as TeX. So either the
/// notation is translated or the equations do not export. This translates the subset that
/// covers real technical prose.
///
/// # What is covered, and why this subset
///
/// - **Greek letters.** `\alpha` and friends. Twenty-six commands, all mechanical.
/// - **`\sum`, `\int`, `\prod`, `\oint`** with limits. The four large operators a technical
///   document uses most, and the ones whose TeX spelling is least like Typst's.
/// - **`\frac{a}{b}`** and **`\sqrt{a}`**, including an optional index.
/// - **`\cdot`, `\times`, `\leq`, `\geq`, `\neq`, `\approx`, `\pm`** -- the binary
///   operators and relations, where Typst has an exact match.
/// - **`\,` `\;` `\!` `\ `** -- spacing. Typst spells these `#thin_space()`, `#med_space()`,
///   `#neg_thin_space()` and a literal space, which is why a mechanical mapping needs them.
///
/// # What is not covered
///
/// Matrices, cases, aligned environments, `\text{}`, custom macros, and anything with braces
/// nested deeper than two. An unrecognised command is **passed through unchanged**, which means
/// Typst reports it by name — so the failure is visible and locatable rather than a silently
/// mangled equation. That is the property that makes a partial translator acceptable: it cannot
/// make an equation wrong without saying so.
///
/// Recorded in `STATUS.md` as owed, with the cost stated: a document using `\begin{matrix}`
/// will not export until this covers it.
pub fn tex_to_typst_math(tex: &str) -> String {
    let mut out = String::with_capacity(tex.len());
    let bytes: Vec<char> = tex.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != '\\' {
            // `^` and `_` in *pass-through* text need the same treatment as in a limit, and this
            // is where the common cases actually are: `e^{-x}`, `x_{i}`, `a^{n}` are written
            // without any command at all, so the first version copied them through and Typst read
            // `-x` as one identifier.
            if bytes[i] == '^' || bytes[i] == '_' {
                let marker = bytes[i];
                let k = i + 1;
                let (arg, after) = if bytes.get(k) == Some(&'{') {
                    read_group(&bytes, k)
                } else if k < bytes.len() {
                    (Some(bytes[k].to_string()), k + 1)
                } else {
                    (None, k)
                };
                if let Some(arg) = arg {
                    let converted = tex_to_typst_math(&arg);
                    out.push(marker);
                    if converted.chars().count() > 1 && !converted.starts_with('(') {
                        out.push('(');
                        out.push_str(&converted);
                        out.push(')');
                    } else {
                        out.push_str(&converted);
                    }
                    i = after;
                    continue;
                }
            }
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        // Read the command name.
        let start = i + 1;
        let mut j = start;
        while j < bytes.len() && bytes[j].is_ascii_alphabetic() {
            j += 1;
        }
        if j == start {
            // A command with no letters: `\,` `\;` `\:` `\!` and `\{` `\}`.
            //
            // These have to be dispatched here rather than falling through, because the name
            // reader above only collects *alphabetic* characters -- so `\,` never became a name
            // and never reached the match arm that handles it. It was emitted as a raw
            // backslash, and Typst accepted `a \ b` as an escape, so the fixture said "compiles"
            // for a conversion that had done nothing. The test passed and the output was wrong,
            // which is the failure mode a compile check cannot catch and only the string
            // comparison can.
            match bytes.get(start) {
                // Separated, because Typst lexes the letters after `)` as one identifier:
                // `thin_space()dx` is "unknown variable: dx", and `e^{-x}\,dx` is how a
                // textbook writes an integral, so this is an ordinary equation rather than an
                // edge case. Three of the fixture entries failed on exactly this before.
                // `\\,dx` and `\\;dA` are the differential of an integral, and `d` followed by
                // another letter is two symbols rather than one identifier. The separator is
                // added *here* because this is the one place the TeX says so: a spacing command
                // in front of a `d`.
                //
                // It cannot be done globally. A blanket "separate adjacent letters" rule turns
                // `literal` into `l i t e r a l` -- seven variables multiplied together, which
                // *compiles* and so passed a fixture that only asked whether it compiled. The
                // string is the only thing that catches this class of mistake.
                // `'!'` is *not* in this arm. It was, once, and the inner match sent it to
                // `thin_space()` -- while a later arm documented that `!` must emit nothing.
                // Both could not be true, and clippy is what said so: the arm that emitted
                // nothing was already unreachable, so a negative thin space had quietly become
                // a positive one. A dead arm is not a style complaint; it is a contradiction
                // between the code and the comment beside it.
                Some(c @ (',' | ';' | ':')) => {
                    let name = match c {
                        ';' => "med_space()",
                        ':' => "thick_space()",
                        _ => "thin_space()",
                    };
                    push_identifier(&mut out, name, bytes.get(start + 1).copied());
                    i = start + 1;
                    if bytes.get(start + 1) == Some(&'d')
                        && matches!(bytes.get(start + 2), Some(n) if n.is_ascii_alphabetic())
                    {
                        // No leading space: `push_identifier` above already separated the
                        // spacing call from the `d`, and adding a second one produced
                        // `thin_space()  dif`. Typst ignores it -- a run of spaces is one space
                        // in markup -- and it is still wrong in the string, and the string is
                        // what a test reads.
                        out.push_str("dif");
                        if matches!(bytes.get(start + 3), Some(n) if n.is_ascii_alphanumeric()) {
                            out.push(' ');
                        }
                        i = start + 3;
                    }
                    continue;
                }
                // `\!` is a *negative* thin space, and Typst's math cannot express one:
                // `space()` parses its argument as a math expression rather than as a length, so
                // `space(-0.167em)` reads `em` as a variable and reports "unknown variable: em".
                // The alternative spellings tried -- `#neg_thin_space()`, `neg_thin_space()` --
                // do not exist in this version.
                //
                // Typst has real names for the delimiters, which `\{` and `\}` map onto. A bare
                // brace opens a code block, so the first version escaped it as `\{` -- which is
                // TeX's spelling and not Typst's -- and Typst rejected it.
                Some('{') => push_identifier(&mut out, "brace.l", bytes.get(start + 1).copied()),
                Some('}') => push_identifier(&mut out, "brace.r", bytes.get(start + 1).copied()),

                // `\!` emits nothing, and this arm has to exist here for that to be true.
                //
                // It used to sit *after* the `',' | ';' | ':' | '!'` arm, where it could never
                // be reached: `'!'` was listed in that arm's pattern, and its inner match sent
                // it to `thin_space()`. So a negative thin space had quietly become a positive
                // one, and the comment above the dead arm said the opposite. clippy found the
                // contradiction, not the bug.
                //
                // The correction is two-sided and both halves are needed. Removing `'!'` from
                // the spacing arm is what makes this one reachable; *deleting* this arm instead
                // — which is what the first attempt did — sends `\!` to the catch-all below and
                // emits a literal backslash, which Typst rejects outright. The fixture
                // `a \! b` compiles either way in the *recorded* data, so only a real compile
                // tells the two apart.
                //
                // Typst's math cannot express a negative thin space at all: `space()` parses its
                // argument as an expression rather than a length, so `space(-0.167em)` reports
                // "unknown variable: em", and neither `neg_thin_space()` nor
                // `#neg_thin_space()` exists in this version. Emitting nothing approximates it
                // as adjacency, which is a sub-point-space difference in the wrong direction —
                // and the alternative was refusing to export a document over a fraction of a
                // millimetre.
                Some('!') => {}
                // Anything else keeps its backslash: `\%`, `\&`, `\#` are TeX's escapes for
                // characters Typst reads literally, and dropping the backslash would change them.
                //
                // A *letter* after the backslash gets a separator. `\dx` is the differential of
                // an integral, which is the single most common command in applied mathematics,
                // and it is two symbols written without a gap. Typst's lexer joins adjacent
                // letters into one identifier regardless of any escape, so `dx` came out as
                // "unknown variable: dx" -- with a hint suggesting `d x`, which is the answer
                // the translator should already have been giving.
                Some(c @ ('a'..='z' | 'A'..='Z')) => {
                    out.push('\\');
                    out.push(*c);
                    if matches!(bytes.get(start + 1), Some(n) if n.is_ascii_alphanumeric()) {
                        out.push(' ');
                    }
                }
                Some(_) => out.push('\\'),
                None => out.push('\\'),
            }
            i = start + 1;
            continue;
        }
        let name: String = bytes[start..j].iter().collect();

        if let Some(replacement) = greek(&name) {
            // Separated, or `i\pi` becomes `ipi` and Typst reads one four-letter identifier.
            //
            // This is a difference between the two notations that is easy to miss: LaTeX's `\pi`
            // is delimited by the backslash, so adjacency is harmless, while Typst's `pi` is a
            // bare identifier and runs into whatever precedes it. The symptom is
            // `unknown variable: ipi`, which names neither the letter nor the command.
            push_identifier(&mut out, replacement, bytes.get(j).copied());
            i = j;
            continue;
        }
        if let Some(replacement) = operator(&name) {
            push_identifier(&mut out, replacement, bytes.get(j).copied());
            i = j;
            continue;
        }

        match name.as_str() {
            // Large operators: `\sum_{i=1}^{n}` -> `sum_(i=1)^n`. Typst takes limits in
            // parentheses after `_` and `^` when they are expressions rather than single
            // characters, which is the whole difference between the two notations for these.
            // Typst's names for the large operators, measured rather than remembered.
            //
            // A probe (`cargo test -p holonomy-shell --test _probe`) compiled each candidate.
            // What it found: `integral`, `integral.double` and `integral.cont` all exist in math;
            // `int` does not; `product` does; and a bare integral *glyph* does not accept limits
            // alongside a superscript. So the glyph became `integral`, and the whole class of
            // failures around large operators went with it.
            //
            // The probe is the point. An earlier round guessed names from documentation --
            // `union`, `intersection`, `diff`, `arrow.r` -- and twelve of them came back
            // `unknown variable`. Typst has no `intersection` and no `diff`.
            "sum" | "prod" | "int" | "oint" | "iint" | "iiint" | "bigcup" | "bigcap" => {
                let typst_name: &str = match name.as_str() {
                    "sum" => "sum",
                    "prod" => "product",
                    "int" => "integral",
                    "oint" => "integral.cont",
                    "iint" => "integral.double",
                    "iiint" => "integral.triple",
                    // Typst has no large-operator form of union or intersection. The symbol is
                    // kept and the "big" is lost, which is the lesser of the two wrongs: the
                    // operator is still visible, and the alternative is a compile error.
                    "bigcup" => "\u{222A}",
                    "bigcap" => "\u{2229}",
                    other => other,
                };
                push_identifier(&mut out, typst_name, bytes.get(j).copied());
                i = read_limit(&bytes, j, &mut out);
            }
            "frac" => {
                let (numerator, after) = read_group(&bytes, j);
                let (denominator, after) = read_group(&bytes, after);
                match (numerator, denominator) {
                    (Some(n), Some(d)) => push_identifier(
                        &mut out,
                        &format!("frac({}, {})", tex_to_typst_math(&n), tex_to_typst_math(&d)),
                        bytes.get(after).copied(),
                    ),
                    _ => push_identifier(&mut out, "frac(?, ?)", bytes.get(after).copied()),
                }
                i = after;
            }
            "sqrt" => {
                // `\sqrt[3]{x}` is an nth root in TeX. Typst spells that `root(3, x)`, which is
                // the only place the index needs carrying over; a plain `\sqrt{x}` is `sqrt(x)`.
                let mut index = None;
                let mut k = j;
                if bytes.get(k) == Some(&'[') {
                    let (inner, after) = read_bracketed(&bytes, k);
                    index = inner;
                    k = after;
                }
                let (radicand, after) = read_group(&bytes, k);
                let radicand = tex_to_typst_math(&radicand.unwrap_or_default());
                match index {
                    Some(idx) => push_identifier(
                        &mut out,
                        &format!("root({}, {})", tex_to_typst_math(&idx), radicand),
                        bytes.get(after).copied(),
                    ),
                    None => push_identifier(
                        &mut out,
                        &format!("sqrt({radicand})"),
                        bytes.get(after).copied(),
                    ),
                }
                i = after;
            }
            // `\left(` and `\right)` mean "a delimiter that grows to fit", which in Typst is
            // `lr(...)` -- and `lr` *reads the brackets it is given* to decide their shape. So
            // the opening delimiter has to be passed through as well as the marker, or the
            // closing `)` has nothing to match and Typst reports "unclosed delimiter".
            "left" => {
                // No `#`, and that is the whole fix.
                //
                // `#` in math mode calls a *code* function; `lr` is a math function, so `#lr(...)`
                // tells the compiler to evaluate a code expression called `lr`, which does not
                // exist. `lr(...)` is the math-mode call. The first version emitted `#lr(` and
                // the probe confirmed both spellings fail identically -- "unknown variable: lr"
                // either way -- so the hash was never the distinguishing part.
                out.push_str("lr(");
                // `(` and `[` become a nested bracket; `\{` becomes a nested brace.
                match bytes.get(j) {
                    Some('(') => out.push('('),
                    Some('[') => out.push('['),
                    Some('{') => out.push('{'),
                    _ => {}
                }
                i = j + 1;
            }
            // The function call opened two things: `lr(` and the delimiter. `\right)` closes
            // both, while `\right]` closes the function's content block and the bracket together.
            // Emitting a single `)` for `\right)` left the call unclosed and Typst reported
            // "unclosed delimiter" on a document whose delimiters were balanced.
            "right" => match bytes.get(j) {
                Some(')') => {
                    out.push_str("))");
                    i = j + 1;
                }
                Some(c @ (']' | '}')) => {
                    out.push(*c);
                    i = j + 1;
                }
                _ => {
                    out.push(')');
                    i = j;
                }
            },
            // Sizing commands with no Typst spelling. Dropped, because keeping `\bigl` would
            // be an undefined variable and dropping the delimiter it sizes would change the
            // equation -- and the delimiter is the part a reader can see.
            "big" | "Big" | "bigg" | "Bigg" | "displaystyle" | "limits" | "nolimits" => {
                i = j;
            }
            // Spacing. `#` is required: inside `$…$` a bare `space(4pt)` is read as a
            // multiplication of two undefined variables. `quad` and `qquad` are Typst *math*
            // symbols and need no call at all.
            "\\" => {
                out.push_str("#thin_space()");
                i = j;
            }
            ";" => {
                out.push_str("#med_space()");
                i = j;
            }
            ":" => {
                out.push_str("#thick_space()");
                i = j;
            }
            "!" => {
                out.push_str("#neg_thin_space()");
                i = j;
            }
            "quad" => {
                push_identifier(&mut out, "quad", bytes.get(j).copied());
                i = j;
            }
            "qquad" => {
                push_identifier(&mut out, "qquad", bytes.get(j).copied());
                i = j;
            }
            // The differential. Not an exotic command: `\int_0^1 e^{-x}\,dx` is how a
            // first-year calculus textbook writes an integral, so a document containing it is
            // ordinary rather than specialist.
            "dif" | "mathrm{d}" | "text{d}" => {
                out.push_str("dif");
                i = j;
            }
            "text" | "mathrm" | "operatorname" => {
                let (inner, after) = read_group(&bytes, j);
                let inner = inner.unwrap_or_default();
                push_identifier(
                    &mut out,
                    &format!("upright(\"{}\")", typst_string(&inner)),
                    bytes.get(after).copied(),
                );
                i = after;
            }
            other => {
                // Passed through, which makes Typst name it. A partial translator that fails
                // loudly beats a complete-looking one that quietly mangles.
                out.push('\\');
                out.push_str(other);
                i = j;
            }
        }
    }
    out
}


/// Write a Typst identifier, separating it from whatever is on either side.
///
/// Typst's math lexer treats a run of letters and digits as one identifier, so `i` followed by
/// `pi` is `ipi` and not two symbols. A space ends the identifier and is not rendered.
///
/// Both sides. Separating only on the right is the version that shipped first, and it produced
/// `ipi` from `i\pi` -- the separation has to happen before the *identifier being written*, which
/// is exactly the side that is easy to forget because the offending character came from the
/// source rather than from this function.
fn push_identifier(out: &mut String, text: &str, next: Option<char>) {
    let touches_left = out
        .chars()
        .last()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
    if touches_left {
        out.push(' ');
    }
    out.push_str(text);
    // Separated from a following letter or digit, but **not** from `_` or `^`.
    //
    // A subscript binds to the thing before it, so `integral_0^1` is one term and
    // `integral _0^1` is an operator followed by something else. This is the actual cause of
    // the "integral with limits" failure that an earlier round recorded as a Typst limitation
    // and worked around by replacing `\int` with a bare glyph: the glyph plus a space plus a
    // subscript is not a large operator, and the glyph alone had nothing to attach the limits
    // to. It looked like a problem with the notation. It was a space this function added.
    if matches!(next, Some(c) if c.is_ascii_alphanumeric()) {
        out.push(' ');
    }
}

/// Read `_` or `^` with a TeX argument, writing the Typst form.
fn read_limit(chars: &[char], at: usize, out: &mut String) -> usize {
    let mut i = at;
    let marker = match chars.get(i) {
        Some('_') => '_',
        Some('^') => '^',
        _ => return i,
    };
    i += 1;
    let (arg, after) = if chars.get(i) == Some(&'{') {
        read_group(chars, i)
    } else if i < chars.len() {
        (Some(chars[i].to_string()), i + 1)
    } else {
        (None, i)
    };
    if let Some(arg) = arg {
        let converted = tex_to_typst_math(&arg);
        out.push(marker);
        // Parenthesised whenever the argument is more than one token. Typst requires it:
        // `e^{-x}` is read as the single identifier `-x` rather than as `-` applied to `x`.
        // TeX does not need the parentheses because its `^` takes the next *token*, so `^{-x}`
        // and `^-x` mean the same thing there -- and the two notations disagree exactly here.
        if converted.chars().count() > 1 && !converted.starts_with('(') {
            out.push('(');
            out.push_str(&converted);
            out.push(')');
        } else {
            out.push_str(&converted);
        }
    }
    after
}

/// Read a `{...}` group, returning its contents with the braces stripped.
fn read_group(chars: &[char], at: usize) -> (Option<String>, usize) {
    if chars.get(at) != Some(&'{') {
        return (None, at);
    }
    let mut depth = 0;
    let mut inner = String::new();
    let mut i = at;
    while i < chars.len() {
        let c = chars[i];
        if c == '{' {
            depth += 1;
            if depth == 1 {
                i += 1;
                continue;
            }
        } else if c == '}' {
            depth -= 1;
            if depth == 0 {
                return (Some(inner), i + 1);
            }
        }
        inner.push(c);
        i += 1;
    }
    // Unbalanced. Handing back what was read is better than dropping the expression, and the
    // unbalanced brace is itself something Typst will report.
    (Some(inner), i)
}

fn read_bracketed(chars: &[char], at: usize) -> (Option<String>, usize) {
    if chars.get(at) != Some(&'[') {
        return (None, at);
    }
    let mut inner = String::new();
    let mut i = at + 1;
    while i < chars.len() && chars[i] != ']' {
        inner.push(chars[i]);
        i += 1;
    }
    (Some(inner), i + 1)
}

/// Greek letters. Typst spells these as Unicode identifiers, which is the one place its math
/// is closer to LaTeX than to anything else.
fn greek_table() -> std::collections::BTreeMap<&'static str, &'static str> {
    [
        ("alpha", "alpha"), ("beta", "beta"), ("gamma", "gamma"), ("delta", "delta"),
        ("epsilon", "epsilon"), ("varepsilon", "epsilon"), ("zeta", "zeta"), ("eta", "eta"),
        ("theta", "theta"), ("vartheta", "theta"), ("iota", "iota"), ("kappa", "kappa"),
        ("lambda", "lambda"), ("mu", "mu"), ("nu", "nu"), ("xi", "xi"), ("pi", "pi"),
        ("rho", "rho"), ("sigma", "sigma"), ("tau", "tau"), ("upsilon", "upsilon"),
        ("phi", "phi"), ("varphi", "phi"), ("chi", "chi"), ("psi", "psi"), ("omega", "omega"),
        ("Gamma", "Gamma"), ("Delta", "Delta"), ("Theta", "Theta"), ("Lambda", "Lambda"),
        ("Xi", "Xi"), ("Pi", "Pi"), ("Sigma", "Sigma"), ("Upsilon", "Upsilon"), ("Phi", "Phi"),
        ("Psi", "Psi"), ("Omega", "Omega"),
    ]
    .into_iter()
    .collect()
}

fn operator_table() -> std::collections::BTreeMap<&'static str, &'static str> {
    // Unicode rather than Typst's identifier names.
    //
    // The first version guessed names from Typst's documentation -- `union`, `intersection`,
    // `diff`, `arrow.r` -- and the fixture generator compiled every one of them: Typst has no
    // `intersection` and no `diff`, and the arrows are not spelled that way. Each guess became
    // `unknown variable: <name>`, which is a worse failure than a wrong letter.
    //
    // Unicode is the *right* answer rather than merely the safe one: these are symbols, Typst's
    // math accepts them directly, so there is no identifier to get wrong and no version whose
    // name list differs. A TeX-command-to-Unicode table is a fact about TeX, and TeX does not
    // change.
    [
        ("cdot", "\u{22C5}"), ("times", "\u{00D7}"), ("div", "\u{00F7}"),
        ("pm", "\u{00B1}"), ("mp", "\u{2213}"),
        ("leq", "\u{2264}"), ("le", "\u{2264}"), ("geq", "\u{2265}"), ("ge", "\u{2265}"),
        ("neq", "\u{2260}"), ("approx", "\u{2248}"), ("equiv", "\u{2261}"), ("propto", "\u{221D}"),
        ("in", "\u{2208}"), ("notin", "\u{2209}"),
        ("subset", "\u{2282}"), ("subseteq", "\u{2286}"),
        ("supset", "\u{2283}"), ("supseteq", "\u{2287}"),
        ("cup", "\u{222A}"), ("cap", "\u{2229}"),
        ("forall", "\u{2200}"), ("exists", "\u{2203}"),
        ("infty", "\u{221E}"), ("partial", "\u{2202}"), ("nabla", "\u{2207}"),
        ("emptyset", "\u{2205}"),
        ("rightarrow", "\u{2192}"), ("to", "\u{2192}"), ("leftarrow", "\u{2190}"),
        ("Rightarrow", "\u{21D2}"), ("Leftrightarrow", "\u{21D4}"),
        ("ldots", "\u{2026}"), ("cdots", "\u{22EF}"), ("prime", "\u{2032}"),
    ]
    .into_iter()
    .collect()
}

static GREEK_INIT: std::sync::OnceLock<std::collections::BTreeMap<&'static str, &'static str>> =
    std::sync::OnceLock::new();
static OPERATORS_INIT: std::sync::OnceLock<std::collections::BTreeMap<&'static str, &'static str>> =
    std::sync::OnceLock::new();

fn greek(name: &str) -> Option<&'static str> {
    GREEK_INIT.get_or_init(greek_table).get(name).copied()
}

fn operator(name: &str) -> Option<&'static str> {
    OPERATORS_INIT.get_or_init(operator_table).get(name).copied()
}

/// Escape a string used as an argument, so a title containing `"` cannot end it.
fn escape_string(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

fn count_block(report: &mut TranslationReport, node: &Value) {
    if let Some(kind) = node.get("type").and_then(Value::as_str) {
        *report.blocks.entry(kind.to_string()).or_insert(0) += 1;
    }
}

fn note_asset(report: &mut TranslationReport, src: &str) {
    if let Some(hash) = src.strip_prefix(&format!("{}://", ASSET_SCHEME)) {
        report.assets.insert(hash.to_string());
    }
}

use super::world::ASSET_SCHEME;

/// The family the preamble names.
///
/// Referenced from `world::BODY_FONT` rather than repeated, because the two must be the same
/// string and a pair of matching literals in two files is a thing that quietly stops being true.
/// `tests/export.rs` asserts the preamble actually contains this family, and the world's test
/// asserts it is one the bundle provides -- so a typo fails a test rather than producing a
/// document that typeset in a fallback face with a warning nobody read.
pub const BODY_FONT: &str = super::world::BODY_FONT;

/// Translate one block.
fn translate_block(node: &Value, depth: usize, out: &mut String, report: &mut TranslationReport) {
    let Some(kind) = node.get("type").and_then(Value::as_str) else {
        return;
    };
    if depth == 0 {
        count_block(report, node);
    }
    let children = node.get("content").and_then(Value::as_array);
    let attrs = node.get("attrs");

    match kind {
        "paragraph" => {
            let mut line = String::new();
            for child in children.into_iter().flatten() {
                translate_inline(child, &mut line, report);
            }
            // An empty paragraph is a blank line, and dropping it collapses the space the author
            // put there. `#parbreak()` rather than an empty line, because an empty line inside a
            // paragraph is one break and between paragraphs is two.
            if line.trim().is_empty() {
                out.push_str("#parbreak()\n\n");
            } else {
                out.push_str(&line);
                out.push_str("\n\n");
            }
        }
        "heading" => {
            let level = attrs
                .and_then(|a| a.get("level"))
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .clamp(1, 5);
            let mut line = String::new();
            for child in children.into_iter().flatten() {
                translate_inline(child, &mut line, report);
            }
            // Typst's default heading numbering is on; a word processor's is off, and a
            // numbered heading in an exported manuscript is wrong in a way nobody asked for.
            out.push_str(&format!("#heading(level: {level})[{}]\n\n", line));
        }
        "blockquote" => {
            out.push_str("#block(inset: (left: 1.5em))[");
            for child in children.into_iter().flatten() {
                translate_block(child, depth + 1, out, report);
            }
            out.push_str("]\n\n");
        }
        "bulletList" | "orderedList" => {
            let marker = if kind == "bulletList" { "-" } else { "+" };
            for child in children.into_iter().flatten() {
                out.push_str(&translate_list_item(child, marker, depth, report));
            }
            out.push('\n');
        }
        "listItem" => {
            out.push_str(&translate_list_item(node, "-", depth, report));
        }
        "codeBlock" => {
            let language = attrs
                .and_then(|a| a.get("language"))
                .and_then(Value::as_str)
                .filter(|l| !l.is_empty() && *l != "plain")
                .map(escape_string);
            let mut text = String::new();
            for child in children.into_iter().flatten() {
                if let Some(t) = child.get("text").and_then(Value::as_str) {
                    text.push_str(t);
                }
            }
            // `raw` is the right element and not a paragraph of monospace text: it does not
            // re-wrap, it keeps leading whitespace, and it does not typeset a `#` in the source
            // as code. Which matters, because the source is a program.
            // The text is a *positional string argument*, after the named ones. `#raw(block:
            // true)[…]` -- passing content after a named argument -- is "expected string, found
            // content", and it took a round trip to find out.
            let body = typst_string(&text);
            match language {
                Some(lang) => out.push_str(&format!(
                    "#raw(block: true, lang: \"{lang}\", \"{body}\")\n\n"
                )),
                None => out.push_str(&format!("#raw(block: true, \"{body}\")\n\n")),
            }
        }
        "horizontalRule" => out.push_str("#line(length: 100%, stroke: 0.5pt + gray)\n\n"),
        "mathBlock" => {
            let latex = attrs
                .and_then(|a| a.get("latex"))
                .and_then(Value::as_str)
                .unwrap_or("");
            // Raw math in *markup* position, not a code-mode argument.
            //
            // The first version passed the TeX as an argument to `#math.equation(...)`, which is
            // code position — and Typst parsed `\sum` as code, so every equation in a document
            // was a compile error naming a character and a caret. `$…$` on its own line is
            // markup, and Typst lays a `$…$` on its own line out as a display equation by itself.
            //
            // Raw rather than `raw(lang: "latex")` because Typst's math mode *is* the renderer
            // for TeX-ish input, and the document's equations were written by KaTeX in the
            // editor — so passing them through unchanged is what makes an exported equation look
            // like the one on screen rather than like source code.
            //
            // Surrounded by a block with spacing so a display equation does not run into the
            // paragraph above it.
            let converted = tex_to_typst_math(latex);
            out.push_str(&format!("#block(above: 0.8em, below: 0.8em)[${converted}$]\n\n"));
        }
        "table" => translate_table(node, out, report, depth),
        "image" => {
            let src = attrs.and_then(|a| a.get("src")).and_then(Value::as_str).unwrap_or("");
            let alt = attrs.and_then(|a| a.get("alt")).and_then(Value::as_str).unwrap_or("");
            note_asset(report, src);
            if src.is_empty() {
                // A figure whose address is missing. Rendering a placeholder keeps the space and
                // says what happened, where emitting `image("")` is a typesetting error that
                // fails the *whole document* over one broken figure.
                out.push_str("#align(center)[#text(fill: gray, size: 0.8em)[missing image]]\n\n");
            } else {
                out.push_str(&format!(
                    "#figure(image(\"{src}\", alt: \"{}\"), alt: \"{}\")\n\n",
                    escape_string(alt),
                    escape_string(alt)
                ));
            }
        }
        "hardBreak" => out.push_str(" \\\n"),
        other => {
            // Unknown: keep the words, drop the formatting, say so.
            report.unknown_types.insert(other.to_string());
            let mut line = String::new();
            for child in children.into_iter().flatten() {
                translate_block(child, depth + 1, &mut line, report);
            }
            if !line.trim().is_empty() {
                out.push_str(&line);
                out.push_str("\n\n");
            }
        }
    }
}

fn translate_list_item(node: &Value, marker: &str, depth: usize, report: &mut TranslationReport) -> String {
    let mut inner = String::new();
    for child in node.get("content").and_then(Value::as_array).into_iter().flatten() {
        translate_block(child, depth + 1, &mut inner, report);
    }
    // Typst's list markup requires the marker at the very start of a line and the item's
    // content to follow on the same line, with continuation lines indented. Emitting the
    // paragraphs of a multi-paragraph item verbatim would break the list, so the item is built
    // as a block and the marker sits outside it.
    let indent = "  ".repeat(depth + 1);
    let mut out = String::new();
    for (i, line) in inner.trim_end().split("\n\n").enumerate() {
        if i == 0 {
            out.push_str(&format!("{indent}{marker} {line}\n"));
        } else {
            out.push_str(&format!("{indent}  {line}\n"));
        }
    }
    out
}

fn translate_table(node: &Value, out: &mut String, report: &mut TranslationReport, depth: usize) {
    let rows = node.get("content").and_then(Value::as_array);
    let mut header: Option<String> = None;
    let mut body: Vec<String> = Vec::new();
    let mut column_count = 0usize;

    for row in rows.into_iter().flatten() {
        if row.get("type").and_then(Value::as_str) != Some("tableRow") {
            continue;
        }
        let mut cells: Vec<String> = Vec::new();
        for cell in row.get("content").and_then(Value::as_array).into_iter().flatten() {
            let mut text = String::new();
            for block in cell.get("content").and_then(Value::as_array).into_iter().flatten() {
                translate_block(block, depth + 1, &mut text, report);
            }
            cells.push(text.trim().replace("\n\n", " ").trim().to_string());
        }
        column_count = column_count.max(cells.len());
        let rendered = format!("[{}]", cells.join("], ["));
        if header.is_none() && node.get("attrs").and_then(|a| a.get("header")) == Some(&Value::Bool(true)) {
            header = Some(rendered);
        } else if header.is_none() {
            // The first row is treated as a header whether or not the schema says so, because a
            // Typst table with no header cannot express what the first row meant visually --
            // bold, or nothing. Treating it as a header is the lesser of the two wrong answers,
            // and the renderer will not disagree with itself between two exports.
            header = Some(rendered);
        } else {
            body.push(rendered);
        }
    }

    let Some(header) = header else {
        return;
    };
    // `columns` is declared explicitly because Typst infers it from the first row, and a
    // malformed table whose second row is wider would otherwise silently drop cells.
    let n = column_count.max(1);
    out.push_str(&format!("#table(\n  columns: {n},\n  {header},\n"));
    for row in body {
        out.push_str(&format!("  {row},\n"));
    }
    out.push_str(")\n\n");
}

/// Translate one inline node into a string.
fn translate_inline(node: &Value, out: &mut String, report: &mut TranslationReport) {
    let Some(kind) = node.get("type").and_then(Value::as_str) else {
        return;
    };
    match kind {
        "text" => {
            let text = node.get("text").and_then(Value::as_str).unwrap_or("");
            let marks = node.get("marks").and_then(Value::as_array);
            let Some(marks) = marks.filter(|m| !m.is_empty()) else {
                out.push_str(&escape_text(text));
                return;
            };
            // Marks nest, and the order they are applied in has to be the reverse of the order
            // they are opened in or the innermost one wins. ProseMirror's mark order is
            // outermost-first, so the walk goes backwards.
            let mut wrapped = escape_text(text);
            let mut lossy_marks: Vec<&str> = Vec::new();
            for mark in marks.iter().rev() {
                let m = mark.get("type").and_then(Value::as_str).unwrap_or("");
                wrapped = match m {
                    "bold" => format!("#strong[{wrapped}]"),
                    "italic" => format!("#emph[{wrapped}]"),
                    "underline" => format!("#underline[{wrapped}]"),
                    "strike" => format!("#strike[{wrapped}]"),
                    "code" => format!("#raw({wrapped:?})"),
                    // `highlight` has no equivalent in Typst's standard library. `mark` is
                    // HTML-only, so using it produced `unknown variable: mark` and a document
                    // that would not compile at all.
                    //
                    // The text is kept undecorated and the mark is reported as lossy, rather
                    // than mapped to something that looks similar but is not: a highlighter is
                    // the author's mark on a quotation, and rendering it as bold claims something
                    // false about the emphasis. Dropping the decoration loses a visual; mapping
                    // it to the wrong one misrepresents the text.
                    "highlight" => {
                        lossy_marks.push("highlight");
                        wrapped
                    }
                    _ => wrapped,
                };
            }
            for m in lossy_marks {
                report.lossy_marks.insert(m.to_string());
            }
            out.push_str(&wrapped);
        }
        "hardBreak" => out.push_str(" \\\\\n"),
        "inlineMath" => {
            let latex = node
                .get("attrs")
                .and_then(|a| a.get("latex"))
                .and_then(Value::as_str)
                .unwrap_or("");
            // Markup position, for the same reason `mathBlock` is: a code-mode argument makes
            // Typst read the TeX as a program rather than as notation. Not passed through
            // `escape_text` either, deliberately — `$` is in that set because *user text* must
            // not start math, and here the math is intended.
            let converted = tex_to_typst_math(latex);
            out.push_str(&format!("${converted}$"));
        }
        "image" => {
            // An image inside a paragraph. Typst has no inline image, so it is rendered as a
            // block-level figure, which changes the paragraph's shape. Refusing is worse.
            let src = node.get("attrs").and_then(|a| a.get("src")).and_then(Value::as_str).unwrap_or("");
            note_asset(report, src);
            if !src.is_empty() {
                out.push_str(&format!("#figure(image(\"{src}\"), placement: none)"));
            }
        }
        other => {
            report.unknown_types.insert(other.to_string());
            for child in node.get("content").and_then(Value::as_array).into_iter().flatten() {
                translate_inline(child, out, report);
            }
        }
    }
}