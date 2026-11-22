//! Grapheme-cluster boundaries: the smallest unit a caret may occupy.
//!
//! A caret is a position *between* clusters, never inside one. That is what makes `👨‍👩‍👧` or
//! `e` + U+0301 behave like one character on screen in hit-testing, and it is one half of the
//! RFC's "cluster-honest carets" (the other half — BiDi reordering — is a later slice).
//!
//! # The rules, not just a table
//!
//! This is an implementation of UAX #29's extended grapheme clusters with no Unicode dependency:
//! the *properties* are data ([`tables`], generated from the database) and the *rules* are code.
//! The structure follows the standard: every code point gets a [`Property`], and [`joins`] applies
//! the rules **in their numbered order**, so each rule's comment can be checked against the
//! specification line by line. The split matters, because the two fail differently: a rule that is
//! wrong is a bug with a test, and a table that is wrong is a hole nobody sees.
//!
//! | rule | what it says |
//! | ---- | ------------ |
//! | GB3 | CR × LF |
//! | GB4, GB5 | break on both sides of a Control, CR or LF |
//! | GB6–GB8 | the Hangul syllable sequences: L×, V×, T× |
//! | GB9 | × Extend, × ZWJ |
//! | GB9a | × SpacingMark |
//! | GB9b | Prepend × |
//! | GB9c | the conjunct run: InCB=Consonant … InCB=Linker … × InCB=Consonant |
//! | GB11 | Pictographic Extend\* ZWJ × Pictographic |
//! | GB12, GB13 | regional indicators pair up, counting from the cluster's start |
//!
//! # The bug this replaced
//!
//! The previous implementation joined **anything** after a ZWJ. That is GB9 (× ZWJ) read as if it
//! were GB11, and it is wrong in the direction that makes a caret *refuse* a position a renderer
//! allows: `a` + ZWJ + `b` is two clusters in UAX #29 (the left context of a joining ZWJ must be
//! Extended_Pictographic), and this said one. The rule is now the standard's, and
//! `a_zwj_joins_only_between_pictographs` pins it.
//!
//! # Coverage, stated exactly
//!
//! Which code point has which property is **not written here**. [`tables`] is generated from the
//! Unicode Character Database (18.0.0) by `scripts/gen-cluster-tables.py`, so every script's marks
//! join their base, not only the ones this UI's spec names by hand. The previous table was typed
//! out a script at a time, and its holes were the quiet kind: a combining mark from a script nobody
//! listed was `Other`, so it split from its base and the split became a caret position. That was
//! AUDIT OQ-11; `a_mark_from_a_script_the_table_never_listed_joins_its_base` pins the difference.
//!
//! The rules are the standard's **through GB13**, including Unicode 15.1's GB9c: a conjunct run is
//! rooted in `InCB=Consonant`, made by an `InCB=Linker`, and continued by `InCB=Extend`, so
//! `क` + VIRAMA + `ष` is one cluster and `क` + `ष` is two. That role is a third property, asked
//! where GB9c asks it and nowhere else (AUDIT D-28).
//!
//! One thing remains **outside** the implementation, and is named rather than implied: **BiDi
//! reordering** — the other half of the RFC's caret claim. It decides paint order rather than
//! segmentation, so it belongs beside the run tree, not here.
//!
//! What is inside it, and was measured rather than assumed (AUDIT D-66): segmentation is already
//! honest for right-to-left scripts. Arabic harakat and Hebrew niqqud are `gcb=Extend` in the
//! generated tables, so a mark joins its base and no caret is offered inside a grapheme;
//! `tests/script_gate.rs` asserts that over fourteen corpus lines in seven RTL scripts. The claim
//! that used to sit here — that RTL text was an untested gap — was about the *cut*, and the cut
//! was right before anyone measured it.
//!
//! What the crate does own is that segmentation is a pure function of the text **and of the state
//! carried into it**: the same point answers the same way however the text was split into windows,
//! which is what L-8 requires.
//!
//! # A window is a piece, and a piece can begin inside a cluster
//!
//! A leaf cut is a byte count, so a window can open on a combining mark whose base is in the window
//! before. [`ClusterContext`] is the two words UAX #29 needs from the text before a piece — the
//! property of its last character, the GB11 run and the regional-indicator count — so a piece
//! handed it segments exactly as the whole document does; [`Clusters::carry`] at the end of one
//! piece is [`Clusters::continuing`] at the start of the next. Without it, a boundary the document
//! does not have became a caret position, and the caret a backspace deleted by stopped at the leaf
//! edge (AUDIT OQ-17).
//!
//! BiDi reordering is **not** here — the other half of the RFC's caret claim — and it changes paint
//! order rather than segmentation, so it belongs beside the run tree.

/// The properties UAX #29's rules read, named as the standard names their classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Property {
    /// Carriage return: GB3 pairs it with a following LF, GB4 breaks after it.
    Cr,
    /// Line feed.
    Lf,
    /// A C0/C1 control, a bidi control, or a line/paragraph separator: breaks on both sides.
    Control,
    /// A non-spacing mark, an enclosing mark, or anything else GB9 attaches to its base.
    Extend,
    /// Zero-width joiner: an Extend for GB9, and the pivot of GB11.
    Zwj,
    /// A regional indicator: two of them make a flag (GB12/GB13).
    RegionalIndicator,
    /// A prepend character: GB9b attaches it to whatever follows.
    Prepend,
    /// A spacing mark (Indic vowel signs and the like): GB9a attaches it to its base.
    SpacingMark,
    /// Hangul choseong (leading consonant).
    HangulL,
    /// Hangul jungseong (vowel).
    HangulV,
    /// Hangul jongseong (trailing consonant).
    HangulT,
    /// A precomposed Hangul syllable with no trailing consonant.
    HangulLv,
    /// A precomposed Hangul syllable with a trailing consonant.
    HangulLvt,
    /// Extended_Pictographic: the left and right context of GB11.
    Pictographic,
    /// Everything else, which joins nothing by default.
    Other,
}

impl Property {
    /// Whether GB4 and GB5 break on both sides of this property.
    const fn is_break(self) -> bool {
        matches!(self, Self::Cr | Self::Lf | Self::Control)
    }
}

/// The property of `character`.
///
/// The table decides every class but the precomposed Hangul syllables, which are arithmetic: LV
/// has a trailing-consonant index of zero and LVT is everything else, so two lines replace the 798
/// ranges the database spells out. The order the old classifier used is gone with the old table —
/// there is nothing left to order, because the classes are disjoint by construction.
fn property(character: char) -> Property {
    if let '\u{AC00}'..='\u{D7A3}' = character {
        let index = u32::from(character) - 0xAC00;
        return if index % 28 == 0 {
            Property::HangulLv
        } else {
            Property::HangulLvt
        };
    }
    tables::break_class(character).unwrap_or(Property::Other)
}

mod tables;

/// The `Indic_Conjunct_Break` role of a character, which GB9c reads (Unicode 15.1).
///
/// It cannot be folded into [`Property`]. `InCB=Consonant` is `Other` to every other rule,
/// `InCB=Extend` is a *subset* of Extend and ZWJ, and since Unicode 18 some `InCB=Linker`
/// characters are neither — so the role is asked separately, and only where GB9c is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Incb {
    /// Not part of a conjunct run.
    None,
    /// `InCB=Consonant`: a consonant that a conjunct run can start from.
    Consonant,
    /// `InCB=Linker`: the virama-like character that turns `Consonant Extend*` into a conjunct.
    Linker,
    /// `InCB=Extend`: continues a run, and can neither start one nor make one.
    Extend,
}

impl Incb {
    /// The role of `character`, whose break class is `property`.
    fn of(character: char, property: Property) -> Self {
        if tables::incb_consonant(character) {
            return Self::Consonant;
        }
        if tables::incb_linker(character) {
            return Self::Linker;
        }
        // InCB=Extend is `[gcb=Extend gcb=ZWJ] - InCB=Linker - InCB=Consonant - U+200C`: a formula
        // over the break class rather than a table, and ZWNJ is the one Extend it excludes by name.
        let extends = matches!(property, Property::Extend | Property::Zwj);
        if extends && character != '\u{200C}' {
            return Self::Extend;
        }
        Self::None
    }
}

/// What GB9c, GB11 and GB12/GB13 need to know about the characters already in the cluster.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Context {
    /// The cluster so far — up to but not including a ZWJ just absorbed — is
    /// `Pictographic Extend*`, which is GB11's left context.
    pictographic: bool,
    /// The character last absorbed was a ZWJ, and the run before it was `Pictographic Extend*`.
    ///
    /// GB11's text is `Pictographic Extend* ZWJ`, so the ZWJ has to remember the run behind it —
    /// and it has to *end* that run. A second ZWJ's left context is a ZWJ, which is not Extend, so
    /// `👨‍‍👩` is two clusters; keeping `pictographic` set across the ZWJ made it one.
    joins_after_zwj: bool,
    /// Regional indicators in the cluster so far, for GB12/GB13's odd/even rule.
    regionals: u32,
    /// The cluster so far ends with `InCB=Consonant [InCB=Extend InCB=Linker]*`, which is GB9c's
    /// left context.
    conjunct: bool,
    /// That run has seen an `InCB=Linker`, without which GB9c does not forbid the break.
    ///
    /// Two flags rather than one because they are decided separately: `क` + `ष` is a run rooted in
    /// a consonant with no linker in it, and it is two clusters.
    linked: bool,
}

impl Context {
    /// Folds one more character into the context.
    fn absorb(&mut self, property: Property, incb: Incb) {
        // Decided before the run is updated: a ZWJ's left context is the run *behind* it.
        self.joins_after_zwj = property == Property::Zwj && self.pictographic;
        self.pictographic = match property {
            Property::Pictographic => true,
            Property::Extend => self.pictographic,
            _ => false,
        };
        self.regionals = if property == Property::RegionalIndicator {
            self.regionals + 1
        } else {
            0
        };
        match incb {
            // A consonant roots a run whether or not the cluster had one: GB9c reads the run that
            // ends here, so only the last consonant in the cluster can root it.
            Incb::Consonant => {
                self.conjunct = true;
                self.linked = false;
            }
            Incb::Linker if self.conjunct => self.linked = true,
            Incb::Extend => {}
            // A linker with no run to be in, and everything else, ends the run.
            _ => {
                self.conjunct = false;
                self.linked = false;
            }
        }
    }
}

/// Whether `current` continues the cluster that ends with `previous`, by UAX #29's rules in order.
///
/// `incb` is the [`Incb`] role of `current`, which only GB9c reads — it is the one rule whose right
/// side is not a break class.
fn joins(previous: Property, current: Property, incb: Incb, context: Context) -> bool {
    use Property::{Cr, Extend, HangulL, HangulLv, HangulLvt, HangulT, HangulV};
    use Property::{Lf, Pictographic, Prepend, RegionalIndicator, SpacingMark, Zwj};

    // GB3: CR × LF.
    if previous == Cr && current == Lf {
        return true;
    }
    // GB4 and GB5: a Control, CR or LF breaks on both sides. This is also what forbids a mark or a
    // joiner from reaching across a line break, which the layout depends on.
    if previous.is_break() || current.is_break() {
        return false;
    }
    // GB6: L × (L | V | LV | LVT).
    // GB7: (LV | V) × (V | T).
    // GB8: (LVT | T) × T.
    match (previous, current) {
        (HangulL, HangulL | HangulV | HangulLv | HangulLvt)
        | (HangulLv | HangulV, HangulV | HangulT)
        | (HangulLvt | HangulT, HangulT) => return true,
        _ => {}
    }
    // GB9: × (Extend | ZWJ).
    if matches!(current, Extend | Zwj) {
        return true;
    }
    // GB9a: × SpacingMark.
    if current == SpacingMark {
        return true;
    }
    // GB9b: Prepend ×.
    if previous == Prepend {
        return true;
    }
    // GB9c: InCB=Consonant [InCB=Extend InCB=Linker]* InCB=Linker [InCB=Extend InCB=Linker]* ×
    // InCB=Consonant. The run must be rooted in a consonant *and* carry a linker, which is what
    // makes `क` + VIRAMA + `ष` one cluster: `क` + `ष` is two, and a ZWNJ between them ends the run.
    if incb == Incb::Consonant && context.conjunct && context.linked {
        return true;
    }
    // GB11: Pictographic Extend* ZWJ × Pictographic.
    if previous == Zwj && context.joins_after_zwj && current == Pictographic {
        return true;
    }
    // GB12 and GB13: an even number of regional indicators in the cluster means the next one starts
    // a new flag.
    if previous == RegionalIndicator && current == RegionalIndicator {
        return context.regionals % 2 == 1;
    }
    false
}

/// An iterator over `(byte offset, cluster text)` pairs.
/// The segmentation state at a cluster boundary: what UAX #29 needs from the text before it.
///
/// A leaf cut is a byte count, so a piece of a document can begin **inside** a cluster — a mark or
/// a joiner at the first byte of a window, with its base at the last byte of the one before.
/// Segmenting that piece from its own first byte would make a boundary a caret position the
/// document does not have (`a` + ZWJ + `b`'s tail, or half a flag), which is AUDIT OQ-17. So the
/// state travels: [`Clusters::carry`] at the end of one piece is [`Clusters::continuing`] at the
/// start of the next, and it is a few flags and a count rather than text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClusterContext {
    /// The property of the last character consumed, which is what GB3–GB13's left context reads.
    previous: Option<Property>,
    /// The GB9c conjunct run, the GB11 run and the GB12/GB13 regional-indicator count *within*
    /// the current cluster.
    context: Context,
}

impl ClusterContext {
    /// The state before the document's first byte: nothing consumed, no cluster open.
    pub const START: Self = Self {
        previous: None,
        context: Context {
            pictographic: false,
            joins_after_zwj: false,
            regionals: 0,
            conjunct: false,
            linked: false,
        },
    };

    /// Whether `character` continues the cluster that ends at this boundary.
    ///
    /// A caller that wants to know if a piece begins inside a cluster asks this about its first
    /// character, which is exactly what the segmenter asks itself.
    #[must_use]
    pub fn joins(self, character: char) -> bool {
        let class = property(character);
        let incb = Incb::of(character, class);
        self.previous
            .is_some_and(|before| joins(before, class, incb, self.context))
    }
}

/// An iterator over the extended grapheme clusters of a text, from the state it was handed.
///
/// [`Clusters::new`] starts at the beginning of a text with nothing before it;
/// [`Clusters::continuing`] starts inside one, told how the text before it ended.
/// [`Clusters::carry`] is the state at the last cluster emitted: hand it to `continuing` and the
/// next piece of the same document segments as the whole document does.
#[derive(Debug, Clone)]
pub struct Clusters<'a> {
    text: &'a str,
    start: usize,
    /// The state at `start`: what the text before this piece said, or the end of the last cluster
    /// this iterator emitted.
    carried: ClusterContext,
}

impl<'a> Clusters<'a> {
    /// Segments `text` from the beginning.
    #[must_use]
    pub fn new(text: &'a str) -> Self {
        Self::continuing(text, ClusterContext::START)
    }

    /// Segments `text` as the continuation of a document that ends at `context`.
    ///
    /// The first cluster of the piece may then begin *before* the piece does, in which case its
    /// boundary inside the piece is not a caret position — the segmenter simply emits it starting
    /// at byte zero, and [`crate::Layout`] keeps it out of the line's boundaries.
    #[must_use]
    pub fn continuing(text: &'a str, context: ClusterContext) -> Self {
        Self {
            text,
            start: 0,
            carried: context,
        }
    }

    /// The state at the end of the text consumed so far — what the next piece needs.
    #[must_use]
    pub fn carry(&self) -> ClusterContext {
        self.carried
    }
}

impl<'a> Iterator for Clusters<'a> {
    type Item = (usize, &'a str);

    fn next(&mut self) -> Option<Self::Item> {
        let start = self.start;
        let text = self.text;
        if start >= text.len() {
            return None;
        }
        let rest = &text[start..];
        let mut end = start;
        let mut previous = self.carried.previous;
        let mut context = self.carried.context;
        // The piece's first character decides whether this cluster is a continuation. If it does
        // not join, the cluster ended before the piece began and this one opens a new cluster —
        // decided once, here, so the loop below can start from a clean state.
        let breaks = rest.chars().next().is_some_and(|first| {
            let class = property(first);
            let incb = Incb::of(first, class);
            previous.is_some_and(|before| !joins(before, class, incb, context))
        });
        if breaks {
            previous = None;
            context = Context::default();
        }
        for (offset, character) in rest.char_indices() {
            let class = property(character);
            let incb = Incb::of(character, class);
            if previous.is_some_and(|before| !joins(before, class, incb, context)) {
                break;
            }
            context.absorb(class, incb);
            previous = Some(class);
            end = start + offset + character.len_utf8();
        }
        self.start = end;
        self.carried = ClusterContext { previous, context };
        Some((start, &text[start..end]))
    }
}

/// How many terminal cells `cluster` occupies (`0` for hard line breaks and bare controls, `2`
/// for East Asian wide/fullwidth clusters, default-emoji-presentation clusters, VS16 emoji
/// sequences and regional-indicator flag pairs, and `1` otherwise — AUDIT OQ-25).
///
/// Both `holonomy_gui::term::CellShaper` and the terminal ANSI renderer consult this single
/// function so cell-grid layout and terminal cursor columns agree by construction. UAX #11
/// `Ambiguous` width is treated as `1` (the standard non-East-Asian recommendation).
#[must_use]
pub fn cluster_cells(cluster: &str) -> u8 {
    let mut chars = cluster.chars();
    let Some(first) = chars.next() else {
        return 0;
    };
    let first_prop = property(first);
    if first_prop.is_break() {
        return 0;
    }
    let mut ri_count = u8::from(first_prop == Property::RegionalIndicator);
    let mut has_vs15 = false;
    let mut has_vs16 = false;
    let mut wide = tables::wide(first);
    for character in chars {
        match character {
            '\u{FE0E}' => {
                has_vs15 = true;
                has_vs16 = false;
            }
            '\u{FE0F}' => {
                has_vs16 = true;
                has_vs15 = false;
            }
            _ => {
                if property(character) == Property::RegionalIndicator {
                    ri_count = ri_count.saturating_add(1);
                }
                if tables::wide(character) {
                    wide = true;
                }
            }
        }
    }
    if ri_count >= 2 {
        return 2;
    }
    if has_vs16 && first_prop == Property::Pictographic {
        return 2;
    }
    if has_vs15 && first_prop == Property::Pictographic {
        return 1;
    }
    if wide {
        2
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clusters(text: &str) -> Vec<&str> {
        Clusters::new(text).map(|(_, cluster)| cluster).collect()
    }

    fn offsets(text: &str) -> Vec<usize> {
        Clusters::new(text).map(|(offset, _)| offset).collect()
    }

    /// How many ranges of the generated table carry `property`.
    fn ranges(property: Property) -> usize {
        tables::BREAK.iter().filter(|r| r.2 == property).count()
    }

    #[test]
    fn ascii_is_one_cluster_per_character() {
        assert_eq!(clusters("abc"), vec!["a", "b", "c"]);
        assert_eq!(offsets("abc"), vec![0, 1, 2]);
    }

    #[test]
    fn a_combining_mark_never_leaves_its_base() {
        // e + COMBINING ACUTE ACCENT is one cluster of three bytes.
        assert_eq!(clusters("e\u{301}x"), vec!["e\u{301}", "x"]);
        assert_eq!(offsets("e\u{301}x"), vec![0, 3]);
    }

    #[test]
    fn variation_selectors_join_their_emoji() {
        // U+2764 U+FE0F is a heart with an emoji presentation selector.
        let text = "\u{2764}\u{FE0F}!";
        assert_eq!(clusters(text), vec!["\u{2764}\u{FE0F}", "!"]);
    }

    #[test]
    fn a_zwj_sequence_is_one_cluster() {
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        assert_eq!(clusters(family), vec![family]);
        assert_eq!(clusters(&format!("{family}x")), vec![family, "x"]);
    }

    #[test]
    fn a_zwj_joins_only_between_pictographs() {
        // GB11 needs Extended_Pictographic on both sides of the joiner. The old implementation
        // joined anything after a ZWJ, which refused a caret position a renderer allows.
        assert_eq!(clusters("a\u{200D}b"), vec!["a\u{200D}", "b"]);
        // A pictograph on the left is enough for the rule, with Extend* in between.
        let with_mark = "\u{1F469}\u{1F3FD}\u{200D}\u{1F4BB}";
        assert_eq!(clusters(with_mark), vec![with_mark]);
        // And a pictograph on the right alone is not: `a` is not the rule's left context.
        assert_eq!(
            clusters("\u{1F600}\u{200D}a"),
            vec!["\u{1F600}\u{200D}", "a"]
        );
    }

    #[test]
    fn a_second_zwj_ends_the_pictographic_run() {
        // GB11's text is `Pictographic Extend* ZWJ` — one joiner at the end of the run. Two
        // joiners put a ZWJ where the Extend* has to be, so the sequence cannot reach across the
        // second one: `👨‍‍👩` is the man-and-joiner cluster and then the woman. The first
        // implementation kept the pictographic flag alive across every ZWJ, which made it one.
        assert_eq!(
            clusters("\u{1F468}\u{200D}\u{200D}\u{1F469}"),
            vec!["\u{1F468}\u{200D}\u{200D}", "\u{1F469}"]
        );
        // One joiner, with Extend* behind it, still joins — the toned sequence is untouched.
        let toned = "\u{1F468}\u{1F3FD}\u{200D}\u{1F469}";
        assert_eq!(clusters(toned), vec![toned]);
        // And a joined pictograph restarts the run, so a chain of them is one cluster: the
        // standard's own family sequence, which the first implementation also got wrong.
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        assert_eq!(clusters(family), vec![family]);
    }

    #[test]
    fn a_piece_that_continues_a_cluster_segments_as_the_whole_text_does() {
        // A leaf cut is a byte count, so a piece can begin inside a cluster. Handed the state the
        // text before it ended in, the piece's first cluster ends where the document's does — at
        // every character boundary of a text that has a mark, a joiner chain, a flag and an Indic
        // conjunct in it (the conjunct's run is state, not a pair, so a cut inside it is the case
        // that tells whether the run travels).
        let conjunct = "\u{0915}\u{094D}\u{0937}";
        let pictographs = "e\u{301}x\u{1F468}\u{200D}\u{1F469}\u{1F1E6}\u{1F1E7}\u{1F1E8}";
        let text = format!("{pictographs}{conjunct}");
        let whole: Vec<(usize, usize)> = Clusters::new(&text)
            .map(|(at, cluster)| (at, at + cluster.len()))
            .collect();
        for cut in 1..text.len() {
            if !text.is_char_boundary(cut) {
                continue;
            }
            let mut prefix = Clusters::new(&text[..cut]);
            while prefix.next().is_some() {}
            let context = prefix.carry();
            let (_, next) = Clusters::continuing(&text[cut..], context)
                .next()
                .expect("a cluster after the cut");
            let expected = whole
                .iter()
                .find(|(at, end)| *at <= cut && cut < *end)
                .map(|(_, end)| *end);
            assert_eq!(
                Some(cut + next.len()),
                expected,
                "a cut at {cut} ends where the document's cluster does"
            );
        }
    }

    #[test]
    fn hangul_jamo_compose_by_the_syllable_rules() {
        // L + V + T is one cluster (GB6, GB7, GB8), and so is a precomposed LV plus a trailing T.
        let conjoining = "\u{1100}\u{1161}\u{11A8}";
        assert_eq!(clusters(conjoining), vec![conjoining]);
        let precomposed = "\u{AC00}\u{11A8}";
        assert_eq!(clusters(precomposed), vec![precomposed]);
        // L + L joins (GB6), and a bare V after an LV joins (GB7), but T + V does not.
        assert_eq!(clusters("\u{1100}\u{1100}"), vec!["\u{1100}\u{1100}"]);
        assert_eq!(clusters("\u{11A8}\u{1161}"), vec!["\u{11A8}", "\u{1161}"]);
    }

    #[test]
    fn a_prepend_character_attaches_to_what_follows_it() {
        // U+0600 ARABIC NUMBER SIGN leads the number it applies to.
        assert_eq!(clusters("\u{0600}\u{0661}a"), vec!["\u{0600}\u{0661}", "a"]);
    }

    #[test]
    fn a_spacing_mark_stays_with_its_base() {
        // Devanagari KA + the spacing vowel sign AA is one cluster.
        assert_eq!(clusters("\u{0915}\u{093E}"), vec!["\u{0915}\u{093E}"]);
    }

    #[test]
    fn a_control_breaks_on_both_sides() {
        // GB4/GB5: a tab is a Control, so nothing joins it from either side.
        assert_eq!(clusters("a\tb"), vec!["a", "\t", "b"]);
        assert_eq!(
            clusters("e\u{301}\u{0007}\u{301}"),
            vec!["e\u{301}", "\u{0007}", "\u{301}"]
        );
    }

    #[test]
    fn a_flag_is_two_regional_indicators() {
        let flag = "\u{1F1F3}\u{1F1F5}";
        assert_eq!(clusters(flag), vec![flag]);
        // A third indicator starts a new cluster instead of joining the finished flag.
        assert_eq!(
            clusters(&format!("{flag}\u{1F1E6}")),
            vec![flag, "\u{1F1E6}"]
        );
    }

    #[test]
    fn a_skin_tone_modifier_joins_and_crlf_is_one_break() {
        assert_eq!(clusters("\u{1F44D}\u{1F3FD}"), vec!["\u{1F44D}\u{1F3FD}"]);
        assert_eq!(clusters("a\r\nb"), vec!["a", "\r\n", "b"]);
        assert_eq!(clusters("a\nb"), vec!["a", "\n", "b"]);
    }

    #[test]
    fn a_break_is_never_joined_by_what_follows_it() {
        // A combining mark after an LF must not become part of the break's cluster: if it did, the
        // cluster would no longer be "\n" and the line break would disappear.
        assert_eq!(clusters("a\n\u{301}b"), vec!["a", "\n", "\u{301}", "b"]);
        // Nor may a joining emoji sequence reach past a newline.
        assert_eq!(clusters("a\u{200D}\nb"), vec!["a\u{200D}", "\n", "b"]);
        // A mark before a break does not drag the break into the cluster either.
        assert_eq!(clusters("e\u{301}\nx"), vec!["e\u{301}", "\n", "x"]);
        // CR LF is still one break; a lone CR is a break by itself.
        assert_eq!(clusters("a\r\nb"), vec!["a", "\r\n", "b"]);
        assert_eq!(clusters("a\rb"), vec!["a", "\r", "b"]);
        assert_eq!(clusters("a\r\u{301}b"), vec!["a", "\r", "\u{301}", "b"]);
    }

    #[test]
    fn an_unassigned_code_point_is_other_and_joins_nothing() {
        // No longer a coverage claim: the table *is* the database. A code point Unicode does not
        // assign has no property, so it is `Other` and stands alone.
        assert_eq!(property('\u{10FFFF}'), Property::Other);
        // A leading mark is still one cluster with the mark after it: GB9 forbids the break, not
        // the start.
        assert_eq!(clusters("\u{301}\u{301}"), vec!["\u{301}\u{301}"]);
    }

    #[test]
    fn a_mark_from_a_script_the_table_never_listed_joins_its_base() {
        // OQ-11's actual failure. The old table was typed out a script at a time, so a combining
        // mark whose script nobody listed was `Other`: it split from its base, and the seam became
        // a caret position the renderer does not have. Kharoshthi and Mende Kikakui are scripts the
        // old list could not have known about.
        assert_eq!(clusters("\u{10A00}\u{10A38}"), vec!["\u{10A00}\u{10A38}"]);
        assert_eq!(clusters("\u{1E900}\u{1E8D0}"), vec!["\u{1E900}\u{1E8D0}"]);
        // Two classes the old approximation got wrong outright: the soft hyphen and the Arabic
        // letter mark are `Cf`, which UAX #29 classes as Control, so they break on both sides
        // instead of joining a neighbour.
        assert_eq!(clusters("a\u{AD}b"), vec!["a", "\u{AD}", "b"]);
        assert_eq!(clusters("a\u{61C}b"), vec!["a", "\u{61C}", "b"]);
    }

    #[test]
    fn the_table_is_sorted_and_disjoint() {
        // The lookup is a binary search, so the table's shape is load-bearing: a regenerated table
        // that overlapped or was out of order would not fail to compile, it would answer wrong.
        let mut previous: Option<char> = None;
        for &(first, last, _) in tables::BREAK {
            assert!(first <= last, "{first:?} > {last:?}");
            if let Some(end) = previous {
                assert!(end < first, "out of order at {first:?}");
            }
            previous = Some(last);
        }
        let mut previous_wide: Option<char> = None;
        for &(first, last) in tables::WIDE_RANGES {
            assert!(first <= last, "{first:?} > {last:?}");
            if let Some(end) = previous_wide {
                assert!(end < first, "wide ranges out of order at {first:?}");
            }
            previous_wide = Some(last);
        }
    }

    #[test]
    fn the_table_is_the_database_and_not_a_sample_of_it() {
        // The counts are Unicode 18.0.0's, minus the 798 precomposed-syllable ranges, which are
        // arithmetic here. Regenerating against another Unicode version moves these numbers with
        // it — which is the point: an upgrade is a decision with a diff, not a side effect.
        assert_eq!(ranges(Property::Cr), 1);
        assert_eq!(ranges(Property::Lf), 1);
        assert_eq!(ranges(Property::Control), 19);
        assert_eq!(ranges(Property::Extend), 390);
        assert_eq!(ranges(Property::Zwj), 1);
        assert_eq!(ranges(Property::RegionalIndicator), 1);
        assert_eq!(ranges(Property::Prepend), 15);
        assert_eq!(ranges(Property::SpacingMark), 158);
        assert_eq!(ranges(Property::HangulL), 2);
        assert_eq!(ranges(Property::HangulV), 4);
        assert_eq!(ranges(Property::HangulT), 2);
        assert_eq!(ranges(Property::Pictographic), 156);
        assert_eq!(tables::BREAK.len(), 750);
        // The two conjunct tables and the OQ-25 wide-range table are pinned the same way.
        assert_eq!(tables::INCB_CONSONANT.len(), 78);
        assert_eq!(tables::INCB_LINKER.len(), 23);
        assert_eq!(tables::WIDE_RANGES.len(), 129);
    }

    #[test]
    fn cluster_cells_assigns_zero_one_and_two_columns_from_the_database() {
        assert_eq!(cluster_cells(""), 0);
        assert_eq!(cluster_cells("\n"), 0);
        assert_eq!(cluster_cells("\r\n"), 0);
        assert_eq!(cluster_cells("a"), 1);
        assert_eq!(cluster_cells("e\u{0301}"), 1);
        assert_eq!(cluster_cells("中"), 2);
        assert_eq!(cluster_cells("한"), 2);
        assert_eq!(cluster_cells("\u{1100}\u{1161}\u{11A8}"), 2);
        assert_eq!(cluster_cells("Ａ"), 2);
        assert_eq!(cluster_cells("🙂"), 2);
        assert_eq!(
            cluster_cells("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}"),
            2
        );
        assert_eq!(cluster_cells("🇯🇵"), 2);
        assert_eq!(cluster_cells("🇯"), 1);
        assert_eq!(cluster_cells("\u{2764}"), 1);
        assert_eq!(cluster_cells("\u{2764}\u{FE0F}"), 2);
        assert_eq!(cluster_cells("\u{231A}\u{FE0E}"), 1);
    }

    #[test]
    fn every_class_the_rules_read_has_code_points_in_it() {
        // A class no character reaches is a rule arm that no text can test. Each one is pinned to a
        // representative here, so the table and the `joins` match arms stay the same set.
        let representatives = [
            ('\r', Property::Cr),
            ('\n', Property::Lf),
            ('\u{0}', Property::Control),
            ('\u{301}', Property::Extend),
            ('\u{200D}', Property::Zwj),
            ('\u{1F1E6}', Property::RegionalIndicator),
            ('\u{600}', Property::Prepend),
            ('\u{903}', Property::SpacingMark),
            ('\u{1100}', Property::HangulL),
            ('\u{1160}', Property::HangulV),
            ('\u{11A8}', Property::HangulT),
            ('\u{AC00}', Property::HangulLv),
            ('\u{AC01}', Property::HangulLvt),
            ('\u{1F600}', Property::Pictographic),
            ('a', Property::Other),
        ];
        for (character, wanted) in representatives {
            assert_eq!(property(character), wanted, "{character:?}");
        }
    }

    #[test]
    fn an_indic_conjunct_is_one_cluster() {
        // GB9c: a consonant, a linker, then the consonant after it is one cluster — the shape of
        // every conjunct in Devanagari and its relatives, and the reason a caret may not sit
        // between the halves of one.
        let devanagari = "\u{0915}\u{094D}\u{0937}";
        assert_eq!(clusters(devanagari), vec![devanagari]);
        let bengali = "\u{0995}\u{09CD}\u{09B7}";
        assert_eq!(clusters(bengali), vec![bengali]);
        // An `InCB=Extend` between the consonant and its linker continues the run rather than
        // ending it: KA + NUKTA + VIRAMA + SSA is still one cluster.
        let nukta = "\u{0915}\u{093C}\u{094D}\u{0937}";
        assert_eq!(clusters(nukta), vec![nukta]);
        // And a joined conjunct carries the run, so a second linker continues it: one cluster.
        let chain = "\u{0915}\u{094D}\u{0937}\u{094D}\u{0930}";
        assert_eq!(clusters(chain), vec![chain]);
    }

    #[test]
    fn a_conjunct_run_needs_both_a_consonant_and_a_linker() {
        // Two consonants with no linker between them are two clusters: GB9c is about the linker,
        // not about consonants touching.
        let bare = vec!["\u{0915}", "\u{0937}"];
        assert_eq!(clusters("\u{0915}\u{0937}"), bare);
        // An Extend alone does not make a conjunct either — KA + NUKTA is one cluster by GB9, and
        // the SSA after it starts a new one.
        let extended = vec!["\u{0915}\u{093C}", "\u{0937}"];
        assert_eq!(clusters("\u{0915}\u{093C}\u{0937}"), extended);
        // A run that does not begin with a consonant does not continue: the standard's GB9c reads
        // `InCB=Consonant [InCB=Extend InCB=Linker]*`, so `a` + VIRAMA + SSA is two clusters even
        // though the virama is a linker. (`unicode-segmentation` suppresses the break here, on
        // having seen any linker; this reads the rule.)
        let unrooted = vec!["a\u{094D}", "\u{0937}"];
        assert_eq!(clusters("a\u{094D}\u{0937}"), unrooted);
    }

    #[test]
    fn a_zwnj_ends_the_conjunct_run() {
        // ZWNJ is `gcb=Extend` but `InCB=None`, and it exists to say "do not form a conjunct": it
        // joins the cluster (GB9) and still ends the run, so the SSA after it is its own cluster.
        let split = vec!["\u{0915}\u{200C}\u{094D}", "\u{0937}"];
        assert_eq!(clusters("\u{0915}\u{200C}\u{094D}\u{0937}"), split);
    }

    #[test]
    fn every_offset_is_a_character_boundary() {
        let text = "a\u{301}b\u{1F600}\u{1F3FD}c\u{1100}\u{1161}\u{11A8}\u{0600}\u{0661}";
        for (offset, cluster) in Clusters::new(text) {
            assert!(text.is_char_boundary(offset));
            assert!(text[offset..].starts_with(cluster));
        }
        // The clusters tile the text exactly: no byte is skipped and none is counted twice.
        let mut covered = 0;
        for (_, cluster) in Clusters::new(text) {
            covered += cluster.len();
        }
        assert_eq!(covered, text.len());
    }
}
