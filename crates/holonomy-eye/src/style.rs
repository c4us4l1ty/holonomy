//! Style runs as layout data, not as a paint-time decoration.
//!
//! A heading is not "the same layout, drawn bigger". A bigger font has bigger advances and a
//! taller line box, so a style change *is* a geometry change — and the moment a renderer applies
//! a style the layout had not been shaped with, that renderer owns a second geometry. That is the
//! v3 disease wearing a hat: paint would look right and the caret would land beside it.
//!
//! So styles live here, above the shaper, in the same run tree everything else walks. The layout
//! asks [`StyledText::style_at`] for each cluster, hands the style to the [`Shaper`] once, and
//! stores the result: `bold`, `italic` and heading sizes are then indistinguishable, as far as this
//! crate is concerned, from any other reason two clusters have different widths.
//!
//! What is deliberately *not* here yet: font families. Choosing a face is a font-pipeline decision
//! (Block 8 owns the pipeline), and inventing an identity scheme for faces before there is a
//! pipeline to interpret it would pin a detail this crate cannot yet test. A run today carries
//! size and the two style flags; when families arrive they join this struct and every consumer
//! already reads it from the run tree.
//!
//! [`Shaper`]: crate::shaper::Shaper

/// The size, in thousandths of a logical unit, that [`Style::DEFAULT`] is defined at.
const SIZE_ONE: i64 = 1000;

/// Everything about a span of text that changes its metrics.
///
/// Sizes are thousandths of a logical unit (14000 = 14 units), because a size is a metric and
/// metrics in this crate are integers: a float size would reintroduce, one zoom tick later, the
/// rounding question [`crate::Fixed`] exists to delete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Style {
    size_milli: i64,
    bold: bool,
    italic: bool,
}

impl Style {
    /// Body text at one logical unit: the size the golden tests and the fake shaper assume.
    pub const DEFAULT: Self = Self {
        size_milli: SIZE_ONE,
        bold: false,
        italic: false,
    };

    /// A regular style at `size_milli` thousandths of a logical unit.
    ///
    /// The size must be positive; a zero size produces zero advances, which collapses every caret
    /// in the run onto one position (the degenerate case the view tests pin deliberately).
    #[must_use]
    pub const fn at(size_milli: i64) -> Self {
        Self {
            size_milli,
            bold: false,
            italic: false,
        }
    }

    /// The same size, bold.
    #[must_use]
    pub const fn bold(self) -> Self {
        Self {
            size_milli: self.size_milli,
            bold: true,
            italic: self.italic,
        }
    }

    /// The same size, italic.
    #[must_use]
    pub const fn italic(self) -> Self {
        Self {
            size_milli: self.size_milli,
            bold: self.bold,
            italic: true,
        }
    }

    /// The size, in thousandths of a logical unit.
    #[must_use]
    pub const fn size_milli(self) -> i64 {
        self.size_milli
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
}

/// One style run: `style` applies from `start` until the next run's start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StyledRun {
    /// Byte offset within the window's text where the run begins.
    pub start: u64,
    /// The style clusters from here onward are shaped with.
    pub style: Style,
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
    ///
    /// Unsorted runs are a programming error, not a data condition: the runs come from the same
    /// spine walk as the text, so they arrive in order. Debug builds assert it rather than silently
    /// picking the last style that happens to match.
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
    /// first run.
    #[must_use]
    pub fn style_at(&self, offset: u64) -> Style {
        let mut style = Style::DEFAULT;
        for run in self.runs {
            if run.start > offset {
                break;
            }
            style = run.style;
        }
        style
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
    fn a_larger_run_is_wider_and_raises_the_line() {
        let runs = [StyledRun {
            start: 3,
            style: Style::at(2000),
        }];
        let styled = StyledText::new("abcDEF", &runs);
        let laid = layout_styled(styled, 0, Fixed::units(40), &Fake::new());
        assert_eq!(laid.lines().len(), 1);
        // `abc` is three one-unit clusters; the doubling starts exactly at byte 3.
        let caret = laid.paint_point(3).expect("the run boundary is a caret");
        assert_eq!(caret.x, Fixed::units(3));
        // A 2× run doubles the line box, because the line is as tall as its tallest run.
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
        // Line 0 ends at the first break, before the run starts: one unit tall.
        assert_eq!(laid.lines()[0].height(), Fixed::units(20));
        // The empty line and everything after it are in the 2× run.
        assert_eq!(laid.lines()[1].height(), Fixed::units(40));
        assert_eq!(laid.lines()[2].top(), Fixed::units(60));
    }
}
