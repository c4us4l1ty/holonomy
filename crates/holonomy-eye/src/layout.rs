//! Laying a window of text out once, so paint and hit-test cannot disagree.
//!
//! [`Layout`] is a tree of lines and placed clusters produced from one window of text, one width
//! and one [`Shaper`]. It is the *only* geometry: [`Layout::hit_test`] maps a point to a caret,
//! [`Layout::paint_point`] maps a caret to a point, and both walk the same
//! [`Line::boundaries`] list built at layout time. GATES law L-8 is the statement that walking
//! them in opposite directions returns what you started with, and it is tested here directly.
//!
//! # What a caret position is
//!
//! A caret sits at a cluster boundary: the start of any cluster, or the **end of a line that ends
//! in a hard break** (a newline, or the end of the text). A soft-wrapped line has no end position
//! of its own — wrapping inserts no character, so the byte after its last cluster is already the
//! first cluster of the next line. Offering both would give one byte two visual positions, and a
//! caret that can be in two places is exactly the drift this block exists to prevent.
//!
//! # What is not here yet
//!
//! BiDi reordering, style runs, word wrapping and the Spine window (which slab supplies the text)
//! are later slices. The layout takes the window's text and its document offset, which is all the
//! arithmetic needs — and all a test needs to build the whole tree.

use crate::cluster::Clusters;
use crate::geom::{Fixed, Point};
use crate::shaper::Shaper;
use crate::style::{Style, StyledText};
use crate::view::View;

/// One cluster, placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placed<'a> {
    /// Document byte offset of the cluster's first byte.
    pub byte: u64,
    /// Left edge, from the window's left margin.
    pub x: Fixed,
    /// Advance width.
    pub advance: Fixed,
    /// The style the cluster was shaped with — the same value both consumers read.
    pub style: Style,
    /// The cluster's text.
    pub text: &'a str,
}

impl Placed<'_> {
    /// The right edge of the cluster.
    #[must_use]
    pub fn end(&self) -> Fixed {
        self.x + self.advance
    }

    /// The document byte offset just past the cluster.
    #[must_use]
    pub fn byte_end(&self) -> u64 {
        self.byte + self.text.len() as u64
    }
}

/// One line of the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line<'a> {
    top: Fixed,
    height: Fixed,
    clusters: Vec<Placed<'a>>,
    start_byte: u64,
    end_byte: u64,
    end_x: Fixed,
    has_end: bool,
}

impl<'a> Line<'a> {
    /// The top of the line's box, from the top of the content.
    #[must_use]
    pub fn top(&self) -> Fixed {
        self.top
    }

    /// The line box height.
    #[must_use]
    pub fn height(&self) -> Fixed {
        self.height
    }

    /// The placed clusters, left to right.
    #[must_use]
    pub fn clusters(&self) -> &[Placed<'a>] {
        &self.clusters
    }

    /// The document byte offset where the line's text starts.
    #[must_use]
    pub fn start_byte(&self) -> u64 {
        self.start_byte
    }

    /// The document byte offset where the line's content ends — before a hard break's newline.
    #[must_use]
    pub fn end_byte(&self) -> u64 {
        self.end_byte
    }

    /// Every caret position on this line, left to right, as `(x, byte)`.
    ///
    /// This is the one list both directions of the geometry walk. A line has a boundary before
    /// each cluster, plus its end position when the line ends in a hard break.
    pub fn boundaries(&self) -> impl Iterator<Item = (Fixed, u64)> + '_ {
        let starts = self
            .clusters
            .iter()
            .map(|cluster| (cluster.x, cluster.byte));
        starts.chain(self.has_end.then_some((self.end_x, self.end_byte)))
    }
}

/// A window of text, laid out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout<'a> {
    lines: Vec<Line<'a>>,
    width: Fixed,
    height: Fixed,
}

impl<'a> Layout<'a> {
    /// The lines, top to bottom.
    #[must_use]
    pub fn lines(&self) -> &[Line<'a>] {
        &self.lines
    }

    /// The width the text was wrapped to.
    #[must_use]
    pub fn width(&self) -> Fixed {
        self.width
    }

    /// The total height of the laid-out content.
    #[must_use]
    pub fn height(&self) -> Fixed {
        self.height
    }

    /// The caret at document byte `byte`, if `byte` is a caret position in this window.
    ///
    /// This is the paint side, in logical units. It is [`View::IDENTITY`]'s answer, which is the
    /// only implementation of the walk: zoom and DPI are a view transform, never a second
    /// geometry, so there is nothing here for a device-space answer to drift from.
    #[must_use]
    pub fn paint_point(&self, byte: u64) -> Option<Caret> {
        View::IDENTITY.paint_point(self, byte)
    }

    /// The caret nearest to `point`, in the same units `paint_point` returns.
    ///
    /// This is the click side, in logical units, and it is [`View::IDENTITY`]'s answer: the
    /// nearest boundary wins, a tie goes to the earlier one so the answer never depends on the
    /// order the clusters happen to be stored in, and `y` is clamped into the content as clicking
    /// past the last line does in every editor.
    #[must_use]
    pub fn hit_test(&self, point: Point) -> Option<Caret> {
        View::IDENTITY.hit_test(self, point)
    }
}

/// A caret: a cluster boundary, placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caret {
    /// Document byte offset of the position.
    pub byte: u64,
    /// Index of the line the caret is on.
    pub line: usize,
    /// Horizontal position of the caret inside the line.
    pub x: Fixed,
    /// Top of the line box.
    pub top: Fixed,
    /// Height of the line box.
    pub height: Fixed,
}

impl Caret {
    /// The point to hand back to [`Layout::hit_test`] and to compare positions with.
    ///
    /// It is the caret's own top-left corner: inside the line box by construction, so the round
    /// trip does not depend on which part of the caret a caller picks.
    #[must_use]
    pub fn point(&self) -> Point {
        Point::new(self.x, self.top)
    }
}

/// Lays plain `text` out at `width`, wrapping between clusters — every cluster at
/// [`Style::DEFAULT`].
///
/// This is [`layout_styled`] with no runs. `origin` is `text`'s document byte offset, so every
/// cluster and caret position is expressed in document coordinates — the same coordinates the
/// Spine and the overlay use. A window that starts at byte 4,096 produces carets at 4,096 and
/// beyond, never at zero.
///
/// Wrapping is between clusters, not between words: word wrapping needs a word model, and a
/// wrong word model would be a second opinion about where text goes.
#[must_use]
pub fn layout_window<'a, S: Shaper + ?Sized>(
    text: &'a str,
    origin: u64,
    width: Fixed,
    shaper: &S,
) -> Layout<'a> {
    layout_styled(StyledText::plain(text), origin, width, shaper)
}

/// Lays styled `text` out at `width`, wrapping between clusters.
///
/// This is the real entry point: a window of text and the runs that style it, in the same shape
/// the Spine hands them over. Each cluster is shaped once, with the style in effect at its first
/// byte, and that style is stored in the run tree — so a renderer never applies a style the layout
/// did not shape with, and a caret never assumes a width the shaper did not report.
///
/// A line box is as tall as its tallest cluster's line height. An empty line — two hard breaks in a
/// row, or the line after a trailing newline — takes the height of the style that reaches it.
#[must_use]
pub fn layout_styled<'a, S: Shaper + ?Sized>(
    styled: StyledText<'a>,
    origin: u64,
    width: Fixed,
    shaper: &S,
) -> Layout<'a> {
    let text = styled.text();
    let mut lines: Vec<Line<'a>> = Vec::new();
    let mut clusters: Vec<Placed<'a>> = Vec::new();
    let mut x = Fixed::ZERO;
    let mut top = Fixed::ZERO;
    let mut start_byte = origin;
    let mut end_byte = origin;
    let mut line_height = Fixed::ZERO;

    for (offset, cluster) in Clusters::new(text) {
        let byte = origin + offset as u64;
        if cluster == "\n" || cluster == "\r\n" {
            // The line's content ends before the break; the next line starts after it.
            let height = filled_height(line_height, styled, offset, shaper);
            lines.push(Line {
                top,
                height,
                clusters: core::mem::take(&mut clusters),
                start_byte,
                end_byte: byte,
                end_x: x,
                has_end: true,
            });
            top += height;
            x = Fixed::ZERO;
            line_height = Fixed::ZERO;
            start_byte = byte + cluster.len() as u64;
            end_byte = start_byte;
            continue;
        }
        let style = styled.style_at(offset as u64);
        let advance = shaper.advance(cluster, style);
        if !clusters.is_empty() && x + advance > width {
            lines.push(Line {
                top,
                height: line_height,
                clusters: core::mem::take(&mut clusters),
                start_byte,
                end_byte: byte,
                end_x: x,
                // A soft wrap has no caret of its own: the next line already starts here.
                has_end: false,
            });
            top += line_height;
            x = Fixed::ZERO;
            line_height = Fixed::ZERO;
            start_byte = byte;
        }
        line_height = line_height.max(shaped_line_height(shaper, style));
        clusters.push(Placed {
            byte,
            x,
            advance,
            style,
            text: cluster,
        });
        x += advance;
        end_byte = byte + cluster.len() as u64;
    }
    let height = filled_height(line_height, styled, text.len(), shaper);
    lines.push(Line {
        top,
        height,
        clusters,
        start_byte,
        end_byte,
        end_x: x,
        has_end: true,
    });
    let total = top + height;
    Layout {
        lines,
        width,
        height: total,
    }
}

/// A line's own height if it has clusters, otherwise the height of the style reaching `offset`.
///
/// An empty line is not a rendered glyph run, so there is no cluster whose height the line could
/// inherit; the style at the line's own position is the only answer the window contains.
fn filled_height<S: Shaper + ?Sized>(
    line_height: Fixed,
    styled: StyledText<'_>,
    offset: usize,
    shaper: &S,
) -> Fixed {
    if line_height.is_zero() {
        let style = styled.style_at(offset as u64);
        return shaped_line_height(shaper, style);
    }
    line_height
}

/// The shaper's line height for `style`, with the invariant hit-testing depends on asserted.
fn shaped_line_height<S: Shaper + ?Sized>(shaper: &S, style: Style) -> Fixed {
    let height = shaper.line_height(style);
    debug_assert!(
        !height.is_zero(),
        "a line height of zero makes hit-testing by y undefined"
    );
    height
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{layout, Fake};

    #[test]
    fn golden_geometry_is_exact() {
        let fake = Fake::new();
        let laid = layout_window("abc", 0, Fixed::units(10), &fake);
        assert_eq!(laid.height(), Fixed::units(20));
        assert_eq!(laid.lines().len(), 1);
        let clusters = laid.lines()[0].clusters();
        assert_eq!(clusters.len(), 3);
        assert_eq!(clusters[0].x, Fixed::ZERO);
        assert_eq!(clusters[1].x, Fixed::ONE);
        assert_eq!(clusters[2].x, Fixed::units(2));
        assert_eq!(clusters[2].end(), Fixed::units(3));
        // The end of the line is a caret position one unit past the last cluster.
        let end = laid.paint_point(3).expect("the end of the text is a caret");
        assert_eq!(end.x, Fixed::units(3));
        assert_eq!(end.byte, 3);
        assert_eq!(end.line, 0);
    }

    #[test]
    fn wrapping_breaks_only_between_clusters() {
        // Three one-unit clusters fit in a three-unit line; the fourth wraps.
        let laid = layout("abcde", 3);
        assert_eq!(laid.lines().len(), 2);
        assert_eq!(laid.lines()[0].clusters().len(), 3);
        assert_eq!(laid.lines()[1].clusters().len(), 2);
        assert_eq!(laid.lines()[1].start_byte(), 3);
        assert_eq!(laid.lines()[1].top(), Fixed::units(20));
        // A soft-wrapped line has no end caret of its own, so byte 3 belongs to line 1 only.
        let caret = laid.paint_point(3).expect("a caret");
        assert_eq!(caret.line, 1);
        assert_eq!(caret.x, Fixed::ZERO);
    }

    #[test]
    fn a_wide_cluster_that_does_not_fit_moves_whole_to_the_next_line() {
        // 'm' is two units: it cannot be split across a one-unit line.
        let laid = layout("am", 1);
        assert_eq!(laid.lines().len(), 2);
        assert_eq!(laid.lines()[0].clusters().len(), 1);
        assert_eq!(laid.lines()[1].clusters()[0].text, "m");
    }

    #[test]
    fn newlines_make_hard_breaks_and_keep_empty_lines() {
        let laid = layout("a\n\nb", 10);
        assert_eq!(laid.lines().len(), 3);
        assert_eq!(laid.lines()[0].clusters().len(), 1);
        assert!(laid.lines()[1].clusters().is_empty());
        assert_eq!(laid.lines()[2].clusters()[0].text, "b");
        // The empty line has exactly one caret position: its own start.
        let empty = laid.paint_point(2).expect("the empty line's caret");
        assert_eq!(empty.line, 1);
        assert_eq!(empty.x, Fixed::ZERO);
        // Byte 1 is the newline itself: the end of the first line.
        let first_end = laid.paint_point(1).expect("the first line's end");
        assert_eq!(first_end.line, 0);
        assert_eq!(first_end.x, Fixed::ONE);
    }

    #[test]
    fn a_click_lands_on_the_nearer_boundary() {
        let laid = layout("abc", 10);
        // Cluster 'b' spans x = 1..2 units; its midpoint is 1.5.
        let before = laid
            .hit_test(Point::new(Fixed::from_64ths(80), Fixed::ZERO))
            .expect("in range");
        assert_eq!(before.byte, 1);
        let after = laid
            .hit_test(Point::new(Fixed::from_64ths(112), Fixed::ZERO))
            .expect("in range");
        assert_eq!(after.byte, 2);
        // A tie between 1 and 2 (x = 1.5 exactly) goes to the earlier boundary.
        let tie = laid
            .hit_test(Point::new(Fixed::from_64ths(96), Fixed::ZERO))
            .expect("in range");
        assert_eq!(tie.byte, 1);
    }

    #[test]
    fn clicking_past_the_end_stays_on_the_last_line() {
        let laid = layout("abc", 10);
        let beyond = laid
            .hit_test(Point::new(Fixed::units(50), Fixed::units(500)))
            .expect("clamped into the content");
        assert_eq!(beyond.byte, 3);
        assert_eq!(beyond.line, 0);
    }

    /// L-8 in miniature: every caret position survives the round trip through the other side.
    fn assert_round_trip(laid: &Layout<'_>, text: &str) {
        let mut checked = 0;
        for byte in 0..=text.len() as u64 {
            let Some(caret) = laid.paint_point(byte) else {
                continue;
            };
            let back = laid
                .hit_test(caret.point())
                .expect("a point the paint side produced must hit-test");
            assert_eq!(back.byte, caret.byte, "byte {byte} moved");
            assert_eq!(back.line, caret.line, "byte {byte} changed lines");
            assert_eq!(back.x, caret.x, "byte {byte} shifted");
            checked += 1;
        }
        assert!(checked > 0, "no caret positions were checked");
    }

    #[test]
    fn every_caret_position_round_trips() {
        for text in [
            "abc",
            "hello world",
            "héllo",
            "e\u{301}x",
            "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
            "a\n\nb",
            "line one\nline two\n",
            "mimimimi",
            "",
        ] {
            for width in [1, 2, 3, 7, 10, 40] {
                let laid = layout(text, width);
                assert_round_trip(&laid, text);
            }
        }
    }

    #[test]
    fn random_texts_round_trip_at_random_widths() {
        const ALPHABET: &str = "abmi \né中\u{301}\u{1F600}\u{1F3FD}\u{200D}";
        let alphabet: Vec<char> = ALPHABET.chars().collect();
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for round in 0..64u64 {
            let length = (next() % 40) as usize;
            let mut text = String::new();
            for _ in 0..length {
                text.push(alphabet[(next() % alphabet.len() as u64) as usize]);
            }
            let width = 1 + (next() % 12) as i64;
            let laid = layout(&text, width);
            assert_round_trip(&laid, &text);
            // Clusters tile each line without overlapping.
            for line in laid.lines() {
                let mut cursor = Fixed::ZERO;
                for cluster in line.clusters() {
                    assert_eq!(cluster.x, cursor, "round {round} in {text:?}");
                    cursor = cluster.end();
                }
            }
        }
    }
}
