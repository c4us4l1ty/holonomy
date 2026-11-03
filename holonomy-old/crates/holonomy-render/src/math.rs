//! Phase 9B: LaTeX math as a micro-AST and a procedural layout.
//!
//! No TeX engine. PROJECT.md §2.3 rejected Typst and §8 rejects a TeX engine, both on binary size, and
//! the arithmetic in §2.9.1 leaves about a mebibyte of headroom against a 2 MiB ceiling. A TeX
//! interpreter is several megabytes before it draws anything. What is here is a parser for four
//! constructs and a layout function, which is a few kilobytes and covers every formula in scope.
//!
//! # What is parsed
//!
//! `\frac{..}{..}`, `\sqrt{..}`, `x^{..}` / `x_{..}`, the named Greek and operator symbols, integers,
//! and `+ - = < > ( ) ,`. Everything else is a parse error with a byte offset, and an error is *not*
//! silently dropped: a formula that does not parse renders as its raw LaTeX in monospace, which is the
//! same thing the editor shows while the caret is inside it. So a parse failure degrades to something
//! legible rather than to nothing.
//!
//! # What is drawn procedurally, and why
//!
//! **The fraction bar and the radical are not glyphs.** Both are rectangles, and §2.9.2 point 4
//! records the reason: a horizontal rule drawn from a glyph outline is antialiased at both ends, so it
//! does not meet the glyphs beside it exactly, and the seam is visible at 1x. A [`MathRun::Rule`] is an
//! integer-aligned fill with no antialiasing at all, and it costs 12 bytes of `MathNode` layout rather
//! than a glyph in a 385-glyph face.
//!
//! Everything else is a glyph from the math face, so the union of codepoints a formula can need is
//! finite and checkable -- see `tests/math_coverage.rs`.
//!
//! # Integers, everywhere
//!
//! [`MathBox`] is `u32` throughout and every sum is saturating. No `f32`, no rounding, no tolerance.
//! The gate asserts hand-computed pixel values, which is only a meaningful assertion if the layout
//! cannot produce anything else.
//!
//! # No allocation after the AST
//!
//! [`layout`] writes into a caller-supplied [`MathLayout`], and [`MathLayout::with_capacity`] exists so
//! a caller can size it once. `measure` allocates nothing at all. This is the "no allocation after the
//! AST is built" requirement, and it is why [`layout`] takes `&mut MathLayout` rather than returning
//! one: returning a `Vec` would allocate inside the function the gate is watching.

use core::fmt;

/// A parsed formula.
///
/// `Box` on the recursive variants, and no `String` in the common path: a node's children live in a
/// `Vec`, which is one allocation per row. That allocation happens during **parsing**, which is a
/// keystroke-rate operation and therefore the one place a per-node `Vec` would hurt -- but the parser
/// is only run when the formula is recompiled, and a formula is a handful of nodes, so the practical
/// count is small and fixed by the input rather than by the document. What must not allocate is the
/// *layout*, and that is enforced by the signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MathNode {
    /// A sequence of nodes, laid out left to right.
    Row(Vec<MathNode>),
    /// One glyph, as a codepoint.
    Symbol(u32),
    /// An integer literal, kept as a number rather than as digits.
    ///
    /// `b^2` and `b^{2}` are the same formula and must lay out identically, and keeping the digits as
    /// glyphs would make them two `Row`s of `Symbol`s that happen to match. Storing the value makes the
    /// equivalence structural.
    Int(i64),
    /// A base with optional scripts.
    SuperSub {
        /// What carries the scripts.
        base: Box<MathNode>,
        /// `x^{..}`, above and to the right.
        sup: Option<Box<MathNode>>,
        /// `x_{..}`, below and to the right.
        sub: Option<Box<MathNode>>,
    },
    /// `\frac{num}{den}`.
    Fraction {
        /// Above the bar.
        num: Box<MathNode>,
        /// Below the bar.
        den: Box<MathNode>,
    },
    /// `\sqrt{radicand}`.
    Sqrt(Box<MathNode>),
}

/// Why a formula did not parse.
///
/// Carries a byte offset into the source, because "unexpected `}` at 14" is actionable and "unexpected
/// token" is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MathError {
    /// The source ended in the middle of something.
    ///
    /// Almost always an unbalanced `{`: `\frac{1}` has no second group.
    UnexpectedEnd {
        /// Byte offset in the source.
        at: usize,
    },
    /// A byte that cannot start or continue anything.
    Unexpected {
        /// Byte offset in the source.
        at: usize,
        /// The byte.
        found: u8,
    },
    /// A closing brace with nothing open.
    UnbalancedClose {
        /// Byte offset in the source.
        at: usize,
    },
    /// A `\` followed by something that is not a known command.
    UnknownCommand {
        /// Byte offset of the backslash.
        at: usize,
        /// How many letters the command's name is.
        name_len: u8,
    },
    /// A command that was recognised but is not in this grammar.
    ///
    /// Separate from [`MathError::UnknownCommand`] because `\quad` *is* a real LaTeX command and a
    /// parser that reports it as unknown is claiming a falsehood. It is out of scope, which is a
    /// different statement.
    UnsupportedCommand {
        /// Byte offset of the backslash.
        at: usize,
        /// How many letters the command's name is.
        name_len: u8,
    },
    /// An integer literal that does not fit an `i64`.
    IntegerTooLarge {
        /// Byte offset where the digits start.
        at: usize,
    },
    /// Nesting deeper than [`MAX_DEPTH`].
    ///
    /// A depth limit rather than a recursion limit, because the parser is recursive and the *renderer*
    /// walks the same tree: a document with 10,000 nested braces would overflow the stack on a
    /// keystroke, and the editor has no way to report that usefully. The limit is generous enough that
    /// no real formula reaches it and small enough that the worst case is bounded.
    TooDeep {
        /// The limit.
        max: usize,
    },
}

/// The deepest nesting the parser will follow. Phase 9B.
pub const MAX_DEPTH: usize = 32;

/// Every symbol this grammar knows, as `(command name, codepoint)`.
///
/// A sorted table so it can be binary-searched, which matters only for tidiness -- a formula has a
/// handful of commands and a linear scan over 60 entries is nothing. It is a `const` array so the
/// table costs no relocation and can be searched from `const` context.
///
/// **This table is the contract between the parser and the font.** `tests/math_coverage.rs` asserts
/// that every codepoint here is present in the math face and in `MATH_RANGES`, so adding a symbol
/// without subsetting the font fails a gate rather than rendering as .notdef.
/// Every symbol this grammar knows, as `(command name, codepoint)`.
///
/// **Sorted by byte value of the name, and that is load-bearing.** `symbol` binary-searches this
/// table, and the first version grouped it by category -- all the lowercase Greek, then the
/// uppercase, then the operators -- which is how a person would write it and is *not* sorted. Binary
/// search on an unsorted table does not fail loudly: it returns `None` for names that are present.
/// `\pm` was in the table and came back unknown, so the quadratic formula did not parse and the gate
/// said `UnknownCommand { at: 9, name_len: 2 }` for a command that is unambiguously there.
///
/// So the table is sorted, `every_symbol_the_grammar_names_resolves_to_a_codepoint` asserts the sorted
/// property against the table itself, and the uppercase Greek sits at the *end* -- after every
/// lowercase name, because `'G'` is 0x47 and `'a'` is 0x61. A reader looking for `\Gamma` will find it
/// in the wrong place, which is the cost of the invariant.
///
/// This table is also the contract between the parser and the font: `tests/math_coverage.rs` asserts
/// every codepoint here is present in the math face and inside `MATH_RANGES`, so a symbol added
/// without subsetting fails a gate rather than rendering as .notdef.
pub const SYMBOLS: &[(&str, u32)] = &[
    ("Delta", 0x394),
    ("Gamma", 0x393),
    ("Lambda", 0x39B),
    ("Omega", 0x3A9),
    ("Phi", 0x3A6),
    ("Pi", 0x3A0),
    ("Psi", 0x3A8),
    ("Sigma", 0x3A3),
    ("Theta", 0x398),
    ("Upsilon", 0x3A5),
    ("Xi", 0x39E),
    ("alpha", 0x3B1),
    ("approx", 0x2248),
    ("beta", 0x3B2),
    ("cdot", 0x22C5),
    ("chi", 0x3C7),
    ("delta", 0x3B4),
    ("div", 0x0F7),
    ("epsilon", 0x3B5),
    ("equiv", 0x2261),
    ("eta", 0x3B7),
    ("exists", 0x2203),
    ("forall", 0x2200),
    ("gamma", 0x3B3),
    ("geq", 0x2265),
    ("in", 0x2208),
    ("infty", 0x221E),
    ("int", 0x222B),
    ("iota", 0x3B9),
    ("kappa", 0x3BA),
    ("lambda", 0x3BB),
    ("leftarrow", 0x2190),
    ("leq", 0x2264),
    ("mp", 0x2213),
    ("mu", 0x3BC),
    ("nabla", 0x2207),
    ("neq", 0x2260),
    ("nu", 0x3BD),
    ("omega", 0x3C9),
    ("omicron", 0x3BF),
    ("partial", 0x2202),
    ("phi", 0x3C6),
    ("pi", 0x3C0),
    ("pm", 0x0B1),
    ("prod", 0x220F),
    ("propto", 0x221D),
    ("psi", 0x3C8),
    ("rho", 0x3C1),
    ("rightarrow", 0x2192),
    ("sigma", 0x3C3),
    ("sum", 0x2211),
    ("tau", 0x3C4),
    ("theta", 0x3B8),
    ("times", 0x0D7),
    ("to", 0x2192),
    ("upsilon", 0x3C5),
    ("xi", 0x3BE),
    ("zeta", 0x3B6),
];

/// Look up a command name, by binary search over [`SYMBOLS`].
pub fn symbol(name: &str) -> Option<u32> {
    SYMBOLS
        .binary_search_by_key(&name, |&(n, _)| n)
        .ok()
        .map(|at| SYMBOLS[at].1)
}

/// Real LaTeX commands that are deliberately **not** in this grammar.
///
/// Listed so the parser can say "out of scope" rather than "I have never heard of it", which is the
/// difference between a useful error and a wrong one. `\left` and `\right` are here because they are
/// the first thing anyone types, and `\left(` is not this grammar's `\left` -- it is a `(` in a group.
pub const OUT_OF_SCOPE: &[&str] = &[
    "quad",
    "qquad",
    "text",
    "mathrm",
    "mathbf",
    "mathit",
    "mathcal",
    "mathbb",
    "left",
    "right",
    "displaystyle",
    "limits",
    "over",
    "choose",
    "binom",
    "frac",
    "sqrt",
    "hat",
    "bar",
    "vec",
    "tilde",
    "dot",
    "ddot",
    "overline",
];

impl fmt::Display for MathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEnd { at } => {
                write!(
                    f,
                    "the formula ended at byte {at} in the middle of something"
                )
            }
            Self::Unexpected { at, found } => write!(
                f,
                "byte {at} holds {:?}, which cannot appear in this grammar",
                *found as char
            ),
            Self::UnbalancedClose { at } => write!(f, "a `}}` at byte {at} with nothing open"),
            // The name's length rather than its bytes: a `Display` impl has only the error, and the
            // name lives in the caller's source. `MathError::name_in` recovers it.
            Self::UnknownCommand { at, name_len } => {
                write!(
                    f,
                    "a {name_len}-letter command at byte {at} is not a command"
                )
            }
            Self::UnsupportedCommand { at, name_len } => write!(
                f,
                "a {name_len}-letter command at byte {at} is real LaTeX but is outside this \
                 grammar's scope"
            ),
            Self::IntegerTooLarge { at } => {
                write!(f, "the integer at byte {at} does not fit in 64 bits")
            }
            Self::TooDeep { max } => {
                write!(f, "nested deeper than {max} levels")
            }
        }
    }
}

impl MathError {
    /// The command's name, recovered from the source it was reported against.
    ///
    /// The error stores a length rather than the bytes because the bytes are a slice of the caller's
    /// buffer and `MathError` has no lifetime. This is the other half of that trade: a caller that
    /// wants to *show* the name passes the same source back in and gets it, with no allocation.
    pub fn name_in<'a>(&self, source: &'a [u8]) -> &'a str {
        let len = match self {
            Self::UnknownCommand { name_len, .. } | Self::UnsupportedCommand { name_len, .. } => {
                *name_len as usize
            }
            _ => return "",
        };
        let start = match self {
            Self::UnknownCommand { at, .. } | Self::UnsupportedCommand { at, .. } => *at + 1,
            _ => return "",
        };
        core::str::from_utf8(source.get(start..start + len).unwrap_or_default()).unwrap_or("")
    }
}

impl std::error::Error for MathError {}

/// Parse a formula's source into a [`MathNode`].
///
/// # Why the source is re-parsed rather than incrementally maintained
///
/// A keystroke inside a formula changes the source, and the cheapest correct thing to do with a changed
/// source is parse it. The formula is a few dozen bytes; a full parse is O(n) in *that*, not in the
/// document, and it cannot produce a tree that disagrees with the text. An incremental parser would be
/// faster and would have a class of bugs where the tree and the text drift, which is the same class of
/// bug the table span map spent Phase 9A fixing.
///
/// `$$` is stripped if present, so the editor can store the delimiters and hand the body straight in.
pub fn parse(source: &[u8]) -> Result<MathNode, MathError> {
    let body = strip_delimiters(source);
    let mut p = Parser {
        src: body,
        at: 0,
        depth: 0,
    };
    let node = p.row(b"")?;
    if p.at < p.src.len() {
        // The only way to stop early is an unmatched `}`, which `row` reports itself.
        return Err(MathError::UnbalancedClose { at: p.at });
    }
    Ok(match node {
        MathNode::Row(items) if items.len() == 1 => items.into_iter().next().expect("one item"),
        other => other,
    })
}

/// Strip a leading `$$` and a trailing `$$`, if both are there.
///
/// Both or neither: a source of `$$x` is a formula whose first character is a dollar sign as far as
/// this function is concerned, because stripping one delimiter would leave the other as content and
/// produce a parse error that points at the wrong byte.
fn strip_delimiters(source: &[u8]) -> &[u8] {
    match (source.strip_prefix(b"$$"), source.strip_suffix(b"$$")) {
        (Some(rest), Some(_)) => rest,
        _ => source,
    }
}

/// The recursive-descent parser.
///
/// Three fields and no lookahead buffer: the grammar is small enough that one byte of lookahead is all
/// any rule needs, and `skip_ws` is the only thing that ever consumes it.
struct Parser<'a> {
    src: &'a [u8],
    at: usize,
    depth: usize,
}

impl<'a> Parser<'a> {
    /// Spaces are skipped everywhere.
    ///
    /// Everywhere, not just between tokens: LaTeX ignores spaces in math mode too, so `a b` is two
    /// symbols with no gap between them, and honouring spaces would make `\frac {1}{2}` parse and
    /// `\frac{1} {2}` not, which is a surprise nobody wants.
    fn skip_ws(&mut self) {
        while self.at < self.src.len() && self.src[self.at].is_ascii_whitespace() {
            self.at += 1;
        }
    }

    /// A sequence of nodes until `}` or the end of the source.
    fn row(&mut self, stop: &[u8]) -> Result<MathNode, MathError> {
        let mut items: Vec<MathNode> = Vec::new();
        loop {
            self.skip_ws();
            if self.at >= self.src.len() {
                break;
            }
            let b = self.src[self.at];
            if stop.contains(&b) {
                break;
            }
            items.push(self.atom()?);
        }
        if items.len() == 1 {
            // One child, not a one-element row: so `x^2` has a `Symbol` base rather than a `Row` of
            // one, and the two spellings are the same tree.
            Ok(items.into_iter().next().expect("one item"))
        } else {
            Ok(MathNode::Row(items))
        }
    }

    /// One atom, plus any `^`/`_` scripts attached to it.
    fn atom(&mut self) -> Result<MathNode, MathError> {
        let base = self.primary()?;
        // Scripts attach to the atom just parsed, and at most one of each: `x^2_3` is both a superscript
        // and a subscript, which is what the node holds.
        let mut sup = None;
        let mut sub = None;
        loop {
            self.skip_ws();
            match self.src.get(self.at) {
                Some(b'^') if sup.is_none() => {
                    self.at += 1;
                    sup = Some(Box::new(self.script_group()?));
                }
                Some(b'_') if sub.is_none() => {
                    self.at += 1;
                    sub = Some(Box::new(self.script_group()?));
                }
                _ => break,
            }
        }
        if sup.is_some() || sub.is_some() {
            Ok(MathNode::SuperSub {
                base: Box::new(base),
                sup,
                sub,
            })
        } else {
            Ok(base)
        }
    }

    /// The argument of a script: one atom if unbraced, a group if braced.
    ///
    /// `b^2` and `b^{2}` are both legal and both mean the same thing, so a script takes a single atom
    /// rather than insisting on braces. That is also why this is not `row`: `b^2x` is `b²x` and must
    /// not swallow the `x`.
    fn script_group(&mut self) -> Result<MathNode, MathError> {
        self.skip_ws();
        if self.src.get(self.at) == Some(&b'{') {
            self.group()
        } else {
            self.primary()
        }
    }

    /// `{ ... }`, consuming the closing brace.
    fn group(&mut self) -> Result<MathNode, MathError> {
        let at = self.at;
        debug_assert_eq!(self.src.get(at), Some(&b'{'));
        self.at += 1;
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(MathError::TooDeep { max: MAX_DEPTH });
        }
        let node = self.row(b"}")?;
        self.depth -= 1;
        if self.src.get(self.at) != Some(&b'}') {
            return Err(MathError::UnexpectedEnd { at });
        }
        self.at += 1;
        Ok(node)
    }

    /// A primary: a group, a command, an integer, a letter, or a punctuation operator.
    fn primary(&mut self) -> Result<MathNode, MathError> {
        let at = self.at;
        match self.src.get(at) {
            None => Err(MathError::UnexpectedEnd { at }),
            Some(b'{') => self.group(),
            Some(b'}') => Err(MathError::UnbalancedClose { at }),
            Some(b'\\') => self.command(),
            Some(&b) if b.is_ascii_digit() => self.integer(),
            Some(&b) if b.is_ascii_alphabetic() => {
                self.at += 1;
                Ok(MathNode::Symbol(u32::from(b)))
            }
            // Punctuation that is its own glyph. `\` is handled above, and a byte that is none of
            // these is reported rather than skipped: a formula containing `&` should say so.
            Some(&b) if b"+-=<>(),!/|".contains(&b) => {
                self.at += 1;
                Ok(MathNode::Symbol(u32::from(b)))
            }
            Some(&b) => Err(MathError::Unexpected { at, found: b }),
        }
    }

    /// Digits, as one [`MathNode::Int`].
    fn integer(&mut self) -> Result<MathNode, MathError> {
        let at = self.at;
        let mut value: i64 = 0;
        while let Some(&b) = self.src.get(self.at) {
            if !b.is_ascii_digit() {
                break;
            }
            value = value
                .checked_mul(10)
                .and_then(|v| v.checked_add(i64::from(b - b'0')))
                .ok_or(MathError::IntegerTooLarge { at })?;
            self.at += 1;
        }
        Ok(MathNode::Int(value))
    }

    /// A `\command`.
    fn command(&mut self) -> Result<MathNode, MathError> {
        let at = self.at;
        self.at += 1;
        let start = self.at;
        while let Some(&b) = self.src.get(self.at) {
            if b.is_ascii_alphabetic() {
                self.at += 1;
            } else {
                break;
            }
        }
        let name = core::str::from_utf8(&self.src[start..self.at]).unwrap_or("");
        let name_len = u8::try_from(name.len()).unwrap_or(u8::MAX);
        if name.is_empty() {
            // `\ `, `\{`, `\\`: a backslash followed by punctuation is an escaped character, which is
            // LaTeX's own rule and the reason `\ ` is a thin space rather than an error.
            if let Some(&b) = self.src.get(self.at) {
                self.at += 1;
                return Ok(MathNode::Symbol(u32::from(b)));
            }
            return Err(MathError::UnexpectedEnd { at });
        }
        if let Some(cp) = symbol(name) {
            return Ok(MathNode::Symbol(cp));
        }
        match name {
            "frac" => {
                let num = self.group()?;
                let den = self.group()?;
                Ok(MathNode::Fraction {
                    num: Box::new(num),
                    den: Box::new(den),
                })
            }
            "sqrt" => Ok(MathNode::Sqrt(Box::new(self.group()?))),
            _ if OUT_OF_SCOPE.contains(&name) => {
                Err(MathError::UnsupportedCommand { at, name_len })
            }
            _ => Err(MathError::UnknownCommand { at, name_len }),
        }
    }
}
