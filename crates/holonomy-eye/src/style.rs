//! Style runs as layout data, not as a paint-time decoration.
//!
//! A heading is not "the same layout, drawn bigger". A bigger font has bigger advances and a
//! taller line box, so a style change *is* a geometry change — and the moment a renderer applies
//! a style the layout had not been shaped with, that renderer owns a second geometry. That is the
//! v3 disease wearing a hat: paint would look right and the caret would land beside it.
//!
//! So styles live here, above the shaper, in the same run tree everything else walks. The layout
//! asks [`StyledText::style_at`] for each cluster, hands the style to the [`Shaper`] once, and
//! stores the result: `bold`, `italic`, family, decorations, headings, alignment, lists, quotes,
//! and links all flow through the single [`Style`] carried on every [`crate::Placed`] cluster and
//! [`crate::PictureCluster`] (`UI-SPEC.md` Law `U0`).
//!
//! [`Shaper`]: crate::shaper::Shaper

use core::num::NonZeroU32;

use holonomy_text::{Alignment, DocStyle, DocStyleSpan, FontFamily, Heading, ListKind};

/// The size, in thousandths of a logical unit, that [`Style::DEFAULT`] is defined at.
const SIZE_ONE: i64 = 1000;

/// Everything about a span of text that shapes its metrics or paints its run (`UI-SPEC.md` §2–§3).
///
/// Sizes are thousandths of a logical unit (`14000` = `14` units), because a size is a metric and
/// metrics in this crate are integers: a float size would reintroduce, one zoom tick later, the
/// rounding question [`crate::Fixed`] exists to delete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Style {
    size_milli: i64,
    family: FontFamily,
    bold: bool,
    italic: bool,
    underline: bool,
    strikethrough: bool,
    code_span: bool,
    heading: Heading,
    alignment: Alignment,
    list: ListKind,
    blockquote: u8,
    horizontal_rule: bool,
    link_id: Option<NonZeroU32>,
}

impl Style {
    /// Body text at one logical unit: the size the golden tests and the fake shaper assume.
    pub const DEFAULT: Self = Self {
        size_milli: SIZE_ONE,
        family: FontFamily::Sans,
        bold: false,
        italic: false,
        underline: false,
        strikethrough: false,
        code_span: false,
        heading: Heading::None,
        alignment: Alignment::Start,
        list: ListKind::None,
        blockquote: 0,
        horizontal_rule: false,
        link_id: None,
    };

    /// A regular style at `size_milli` thousandths of a logical unit.
    #[must_use]
    pub const fn at(size_milli: i64) -> Self {
        Self {
            size_milli,
            ..Self::DEFAULT
        }
    }

    /// Converts a Spine [`DocStyle`] into its layout [`Style`], resolving heading/code effective
    /// sizes, weights, and families (`UI-SPEC.md` §2).
    #[must_use]
    pub const fn from_doc(doc: DocStyle) -> Self {
        Self {
            size_milli: doc.effective_size_milli() as i64,
            family: doc.effective_family(),
            bold: doc.effective_bold(),
            italic: doc.italic,
            underline: doc.underline,
            strikethrough: doc.strikethrough,
            code_span: doc.code_span,
            heading: doc.heading,
            alignment: doc.alignment,
            list: doc.list,
            blockquote: doc.blockquote,
            horizontal_rule: doc.horizontal_rule,
            link_id: doc.link_id,
        }
    }

    /// The same style, bold.
    #[must_use]
    pub const fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    /// The same style, italic.
    #[must_use]
    pub const fn italic(mut self) -> Self {
        self.italic = true;
        self
    }

    /// The same style, underlined.
    #[must_use]
    pub const fn underline(mut self) -> Self {
        self.underline = true;
        self
    }

    /// The same style, struck through.
    #[must_use]
    pub const fn strikethrough(mut self) -> Self {
        self.strikethrough = true;
        self
    }

    /// Sets the font family.
    #[must_use]
    pub const fn with_family(mut self, family: FontFamily) -> Self {
        self.family = family;
        self
    }

    /// Sets the paragraph heading level and updates size/bold when still at body defaults.
    #[must_use]
    pub const fn with_heading(mut self, heading: Heading) -> Self {
        self.heading = heading;
        if self.size_milli == SIZE_ONE && !matches!(heading, Heading::None) {
            self.size_milli = heading.default_size_milli() as i64;
        }
        if heading.is_bold_by_default() {
            self.bold = true;
        }
        self
    }

    /// Sets the paragraph horizontal alignment.
    #[must_use]
    pub const fn with_alignment(mut self, alignment: Alignment) -> Self {
        self.alignment = alignment;
        self
    }

    /// Sets the structural list membership.
    #[must_use]
    pub const fn with_list(mut self, list: ListKind) -> Self {
        self.list = list;
        self
    }

    /// Sets the blockquote nesting depth (`clamped <= 8`).
    #[must_use]
    pub const fn with_blockquote(mut self, depth: u8) -> Self {
        self.blockquote = if depth > 8 { 8 } else { depth };
        self
    }

    /// Marks this run as an inline monospace code span.
    #[must_use]
    pub const fn code_span(mut self) -> Self {
        self.code_span = true;
        if matches!(self.family, FontFamily::Sans) {
            self.family = FontFamily::Mono;
        }
        if self.size_milli == SIZE_ONE {
            self.size_milli = 900;
        }
        self
    }

    /// Marks this run as a horizontal-rule divider.
    #[must_use]
    pub const fn horizontal_rule(mut self) -> Self {
        self.horizontal_rule = true;
        self
    }

    /// Attaches a 1-based link-table reference.
    #[must_use]
    pub const fn with_link_id(mut self, link_id: Option<NonZeroU32>) -> Self {
        self.link_id = link_id;
        self
    }

    /// The size, in thousandths of a logical unit.
    #[must_use]
    pub const fn size_milli(self) -> i64 {
        self.size_milli
    }

    /// Font family (`Sans`, `Serif`, `Mono`).
    #[must_use]
    pub const fn family(self) -> FontFamily {
        self.family
    }

    /// Whether the run is bold.
    #[must_use]
    pub const fn is_bold(self) -> bool {
        self.bold
    }

    /// Whether the run is italic.
    #[must_use]
    pub const fn is_italic(self) -> bool {
        self.italic
    }

    /// Whether the run is underlined.
    #[must_use]
    pub const fn is_underline(self) -> bool {
        self.underline
    }

    /// Whether the run is struck through.
    #[must_use]
    pub const fn is_strikethrough(self) -> bool {
        self.strikethrough
    }

    /// Whether the run is an inline code span.
    #[must_use]
    pub const fn is_code_span(self) -> bool {
        self.code_span
    }

    /// Paragraph heading level.
    #[must_use]
    pub const fn heading(self) -> Heading {
        self.heading
    }

    /// Paragraph horizontal alignment.
    #[must_use]
    pub const fn alignment(self) -> Alignment {
        self.alignment
    }

    /// Structural list membership.
    #[must_use]
    pub const fn list(self) -> ListKind {
        self.list
    }

    /// Blockquote nesting depth (`0..=8`).
    #[must_use]
    pub const fn blockquote(self) -> u8 {
        self.blockquote
    }

    /// Whether this run represents a horizontal rule.
    #[must_use]
    pub const fn is_horizontal_rule(self) -> bool {
        self.horizontal_rule
    }

    /// Optional 1-based link-table index.
    #[must_use]
    pub const fn link_id(self) -> Option<NonZeroU32> {
        self.link_id
    }

    /// Structural left indent in whole logical units (`2` units per list or blockquote depth).
    #[must_use]
    pub const fn indent_units(self) -> i64 {
        2 * (self.list.depth() as i64 + self.blockquote as i64)
    }
}

/// One style run: `style` applies from `start` until the next run's start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StyledRun {
    /// Byte offset within the window's text where the run begins.
    pub start: u64,
    /// The style clusters from here onward are shaped with.
    pub style: Style,
}

impl StyledRun {
    /// Converts sorted, non-overlapping Spine [`DocStyleSpan`]s overlapping
    /// `[origin, origin + window_len)` into window-relative change-point [`StyledRun`]s.
    #[must_use]
    pub fn from_doc_spans(spans: &[DocStyleSpan], origin: u64, window_len: u64) -> Vec<Self> {
        if spans.is_empty() || window_len == 0 {
            return Vec::new();
        }
        let window_end = origin.saturating_add(window_len);
        let mut out: Vec<Self> = Vec::with_capacity(spans.len() * 2);
        let mut cursor = origin;
        let mut active = Style::DEFAULT;

        for span in spans {
            let s = span.start.max(origin);
            let e = span.end.min(window_end);
            if s >= e {
                continue;
            }
            if cursor < s && active != Style::DEFAULT {
                out.push(Self {
                    start: cursor - origin,
                    style: Style::DEFAULT,
                });
                active = Style::DEFAULT;
            }
            let next_style = Style::from_doc(span.style);
            if next_style != active {
                out.push(Self {
                    start: s - origin,
                    style: next_style,
                });
                active = next_style;
            }
            cursor = e;
        }
        if cursor < window_end && active != Style::DEFAULT {
            out.push(Self {
                start: cursor - origin,
                style: Style::DEFAULT,
            });
        }
        out
    }
}

/// A window of text together with the runs that style it.
///
/// Offsets are **window-relative**, not document-absolute, because that is the coordinate space the
/// text arrived in: the Eye is handed one viewport window and the runs that intersect it, never the
/// document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StyledText<'a> {
    text: &'a str,
    runs: &'a [StyledRun],
}

impl<'a> StyledText<'a> {
    /// Text with no runs: every cluster is [`Style::DEFAULT`].
    #[must_use]
    pub const fn plain(text: &'a str) -> Self {
        Self { text, runs: &[] }
    }

    /// Text with `runs`, which must be sorted by `start` and start at most once per byte.
    #[must_use]
    pub fn new(text: &'a str, runs: &'a [StyledRun]) -> Self {
        debug_assert!(
            runs.windows(2).all(|pair| pair[0].start < pair[1].start),
            "style runs must be sorted by start and unique"
        );
        Self { text, runs }
    }

    /// The window's text.
    #[must_use]
    pub const fn text(self) -> &'a str {
        self.text
    }

    /// The window's runs, as given.
    #[must_use]
    pub const fn runs(self) -> &'a [StyledRun] {
        self.runs
    }

    /// The style in effect at window-relative byte `offset`, or [`Style::DEFAULT`] before the
    /// first run (`O(log R)` binary search).
    #[must_use]
    pub fn style_at(&self, offset: u64) -> Style {
        let idx = self.runs.partition_point(|run| run.start <= offset);
        if idx == 0 {
            Style::DEFAULT
        } else {
            self.runs[idx - 1].style
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Fixed;
    use crate::layout::layout_styled;
    use crate::test_support::Fake;

    #[test]
    fn style_at_walks_the_runs_in_order() {
        let runs = [
            StyledRun {
                start: 2,
                style: Style::DEFAULT.bold(),
            },
            StyledRun {
                start: 5,
                style: Style::at(2000).italic(),
            },
        ];
        let styled = StyledText::new("abcdefgh", &runs);
        assert_eq!(styled.style_at(0), Style::DEFAULT);
        assert_eq!(styled.style_at(2), Style::DEFAULT.bold());
        assert_eq!(styled.style_at(4), Style::DEFAULT.bold());
        assert_eq!(styled.style_at(5), Style::at(2000).italic());
        assert_eq!(styled.text(), "abcdefgh");
        assert_eq!(styled.runs().len(), 2);
        assert_eq!(StyledText::plain("x").style_at(0), Style::DEFAULT);
    }

    #[test]
    fn from_doc_spans_inserts_default_reset_after_bounded_span() {
        let mut bold_doc = DocStyle::DEFAULT;
        bold_doc.bold = true;
        let spans = [DocStyleSpan {
            start: 12,
            end: 16,
            style: bold_doc,
        }];
        let runs = StyledRun::from_doc_spans(&spans, 10, 10);
        let styled = StyledText::new("0123456789", &runs);
        assert_eq!(styled.style_at(1), Style::DEFAULT);
        assert_eq!(styled.style_at(2), Style::DEFAULT.bold());
        assert_eq!(styled.style_at(5), Style::DEFAULT.bold());
        assert_eq!(styled.style_at(6), Style::DEFAULT);
    }

    #[test]
    fn a_larger_run_is_wider_and_raises_the_line() {
        let runs = [StyledRun {
            start: 3,
            style: Style::at(2000),
        }];
        let styled = StyledText::new("abcDEF", &runs);
        let laid = layout_styled(styled, 0, Fixed::units(40), &Fake::new());
        assert_eq!(laid.lines().len(), 1);
        let caret = laid.paint_point(3).expect("the run boundary is a caret");
        assert_eq!(caret.x, Fixed::units(3));
        assert_eq!(laid.lines()[0].height(), Fixed::units(40));
        assert_eq!(laid.height(), Fixed::units(40));
        let placed = laid.lines()[0].clusters();
        assert_eq!(placed[3].style, Style::at(2000));
        assert_eq!(placed[3].advance, Fixed::units(2));
        assert_eq!(placed[2].style, Style::DEFAULT);
    }

    #[test]
    fn a_style_boundary_does_not_open_a_caret_seam() {
        let runs = [
            StyledRun {
                start: 0,
                style: Style::at(1500).bold(),
            },
            StyledRun {
                start: 4,
                style: Style::at(900),
            },
            StyledRun {
                start: 9,
                style: Style::at(2400).italic(),
            },
        ];
        let text = "one two three";
        let styled = StyledText::new(text, &runs);
        let laid = layout_styled(styled, 0, Fixed::units(6), &Fake::new());
        assert!(laid.lines().len() > 2);
        let mut checked = 0;
        for byte in 0..=text.len() as u64 {
            let Some(caret) = laid.paint_point(byte) else {
                continue;
            };
            let back = laid.hit_test(caret.point()).expect("its own point");
            assert_eq!(back.byte, byte);
            assert_eq!(back.line, caret.line);
            checked += 1;
        }
        assert!(checked > 0);
    }

    #[test]
    fn an_empty_line_takes_the_style_that_reaches_it() {
        let runs = [StyledRun {
            start: 2,
            style: Style::at(2000),
        }];
        let styled = StyledText::new("a\n\nb", &runs);
        let laid = layout_styled(styled, 0, Fixed::units(40), &Fake::new());
        assert_eq!(laid.lines().len(), 3);
        assert_eq!(laid.lines()[0].height(), Fixed::units(20));
        assert_eq!(laid.lines()[1].height(), Fixed::units(40));
        assert_eq!(laid.lines()[2].top(), Fixed::units(60));
    }
}
