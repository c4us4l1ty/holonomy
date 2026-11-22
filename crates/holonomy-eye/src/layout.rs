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
//! # Direction and wrapping
//!
//! Paragraph embedding levels and explicit/weak/neutral runs follow UAX #9 (`bidi.rs`). Lines wrap
//! greedily between clusters in logical order first, and each wrapped line reorders its clusters
//! (`L1.4`, `L2`) into visual left-to-right order while pairing every logical cluster boundary
//! bijectively onto the line's visual boundary positions so L-8 holds across LTR, RTL, and mixed
//! bidirectional text (AUDIT OQ-11). Word-level break opportunities remain a separate concern.

use crate::bidi::{reorder_line, BidiClass, ResolvedParagraph, WindowBidi};
use crate::cluster::{ClusterContext, Clusters};
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
    bidi_boundaries: Option<Vec<(Fixed, u64)>>,
    start_byte: u64,
    end_byte: u64,
    end_x: Fixed,
    has_end: bool,
    /// The line's first cluster began **before** this window, in a window laid out as a
    /// continuation — so the window's first byte is inside a cluster and is not a caret position
    /// (AUDIT OQ-17). Only ever set on the first line.
    continues_cluster: bool,
}

enum BoundaryIter<'a> {
    Logical {
        clusters: core::slice::Iter<'a, Placed<'a>>,
        end: Option<(Fixed, u64)>,
    },
    Reordered(core::slice::Iter<'a, (Fixed, u64)>),
}

impl Iterator for BoundaryIter<'_> {
    type Item = (Fixed, u64);

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Logical { clusters, end } => clusters
                .next()
                .map(|cluster| (cluster.x, cluster.byte))
                .or_else(|| end.take()),
            Self::Reordered(iter) => iter.next().copied(),
        }
    }
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

    /// Where the line's text ends: the x after its last cluster, or `start_x` for a line this
    /// window only inherits the tail of.
    ///
    /// A caller that lays a document out in pieces carries this value into
    /// [`layout_styled_from`] for the next piece — it is the line's position, not the window's.
    #[must_use]
    pub fn end_x(&self) -> Fixed {
        self.end_x
    }

    /// Whether the line offers a caret at its end.
    ///
    /// True for a line that ends in a hard break, and true for the **document's last line** even
    /// when no break follows it — a document's end is a caret position, and a layout that withheld
    /// it would leave the last glyph with no caret to its right. False for a line a window inherits
    /// the tail of, whose own caret was offered by the window that ended it. (This used to say a
    /// line offers its end caret "when the line ends in a hard break" and nothing else — a
    /// comment a Block 5 gate harness contradicted by counting the last document line's extra
    /// boundary. The code was right; the sentence was wrong.)
    #[must_use]
    pub fn has_end(&self) -> bool {
        self.has_end
    }

    /// Every caret position on this line, left to right, as `(x, byte)`.
    ///
    /// This is the one list both directions of the geometry walk. A line has a boundary before
    /// each cluster, plus its end position when the line ends in a hard break.
    pub fn boundaries(&self) -> impl Iterator<Item = (Fixed, u64)> + '_ {
        if let Some(ref bounds) = self.bidi_boundaries {
            return BoundaryIter::Reordered(bounds.iter());
        }
        let skip = usize::from(self.continues_cluster);
        BoundaryIter::Logical {
            clusters: self.clusters[skip.min(self.clusters.len())..].iter(),
            end: self.has_end.then_some((self.end_x, self.end_byte)),
        }
    }

    /// The x where the cluster holding `byte` begins, or the line's own start when `byte` is before
    /// its first cluster.
    ///
    /// A selection paints whole clusters, so the answer for a byte inside a cluster is that
    /// cluster's left edge, and the answer for a byte at or past the line's content is where the
    /// line's content ends.
    #[must_use]
    pub fn x_at(&self, byte: u64) -> Fixed {
        if byte >= self.end_byte {
            return self.end_x;
        }
        if self.bidi_boundaries.is_some() {
            return self
                .clusters
                .iter()
                .filter(|cluster| cluster.byte <= byte)
                .max_by_key(|cluster| cluster.byte)
                .or_else(|| self.clusters.iter().min_by_key(|cluster| cluster.byte))
                .map_or(Fixed::ZERO, |cluster| cluster.x);
        }
        let mut x = self
            .clusters
            .first()
            .map_or(Fixed::ZERO, |cluster| cluster.x);
        for cluster in &self.clusters {
            if cluster.byte > byte {
                break;
            }
            x = cluster.x;
        }
        x
    }

    /// The x just past the cluster holding `byte` — a cluster is painted whole — or the line's end
    /// when `byte` is at or past its last cluster.
    #[must_use]
    pub fn x_after(&self, byte: u64) -> Fixed {
        if self.bidi_boundaries.is_some() {
            return self
                .clusters
                .iter()
                .filter(|cluster| byte < cluster.byte_end())
                .min_by_key(|cluster| cluster.byte)
                .map_or(self.end_x, |cluster| cluster.end());
        }
        for cluster in &self.clusters {
            if byte < cluster.byte_end() {
                return cluster.end();
            }
        }
        self.end_x
    }
}

/// A window of text, laid out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout<'a> {
    lines: Vec<Line<'a>>,
    width: Fixed,
    height: Fixed,
    carry: ClusterContext,
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

    /// The segmentation state at the end of the window's text: what the next piece of the document
    /// must be handed as [`layout_styled_continuing`]'s `context`.
    ///
    /// A caller that lays a document out in pieces carries this beside the line position, which is
    /// what makes a piece's first cluster continue the one before it instead of starting a new one
    /// (AUDIT OQ-17).
    #[must_use]
    pub fn carry(&self) -> ClusterContext {
        self.carry
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
    layout_styled_from(styled, origin, Fixed::ZERO, width, shaper)
}

/// Lays styled `text` out as the **continuation of a line** that is already `start_x` wide.
///
/// This is what makes a document's geometry independent of where its leaves happen to fall. A leaf
/// boundary can land in the middle of a paragraph, and laying the leaf out from zero would wrap its
/// text somewhere the document does not — so a caller that knows where the previous window's last
/// line ended hands that position in, and gets the same breaks the document has.
///
/// Two things follow, and both are deliberate:
///
/// * The first line of the result is the tail of the carried line: `top` is zero, `start_byte` is
///   `origin`, and it is **not** a line of its own — a caller summing lines per leaf skips it.
/// * If the first cluster does not fit at `start_x`, the carried line ends there (empty, with no
///   caret) and the cluster starts a new line. That is the same greedy rule the layout uses
///   everywhere: a line ends when the next cluster would not fit, and the next line starts at it.
///
/// With `start_x == Fixed::ZERO` this is exactly [`layout_styled`], line for line.
#[must_use]
pub fn layout_styled_from<'a, S: Shaper + ?Sized>(
    styled: StyledText<'a>,
    origin: u64,
    start_x: Fixed,
    width: Fixed,
    shaper: &S,
) -> Layout<'a> {
    layout_styled_continuing(
        styled,
        origin,
        start_x,
        ClusterContext::START,
        width,
        shaper,
    )
}

/// Lays styled text out as the continuation of a **cluster** as well as of a line.
///
/// This is [`layout_styled_from`] with the segmentation state a piece of a document needs when it
/// begins inside a cluster: `context` is the previous piece's [`Layout::carry`], and the result's
/// [`Layout::carry`] is this piece's. The first cluster may then begin before `origin`, and the
/// first line will not offer `origin` as a caret position — because the document does not.
///
/// # Errors
///
/// None: segmentation never fails. A piece that begins mid-cluster is laid out with the cluster it
/// belongs to, however much of it the shaper is given.
#[must_use]
pub fn layout_styled_continuing<'a, S: Shaper + ?Sized>(
    styled: StyledText<'a>,
    origin: u64,
    start_x: Fixed,
    context: ClusterContext,
    width: Fixed,
    shaper: &S,
) -> Layout<'a> {
    let text = styled.text();
    let opens_inside_a_cluster = text
        .chars()
        .next()
        .is_some_and(|first| context.joins(first));
    let mut lines: Vec<Line<'a>> = Vec::new();
    let mut clusters: Vec<Placed<'a>> = Vec::new();
    let mut x = start_x;
    let mut top = Fixed::ZERO;
    let mut start_byte = origin;
    let mut end_byte = origin;
    let mut line_height = Fixed::ZERO;
    // Whether the line the next cluster lands on already has content — in this window or, for the
    // first cluster, in the window before it. A line never ends before its first cluster.
    let mut continued = !start_x.is_zero();

    let mut segments = Clusters::continuing(text, context);
    for (offset, cluster) in segments.by_ref() {
        let byte = origin + offset as u64;
        if cluster == "\n" || cluster == "\r" || cluster == "\r\n" {
            // The line's content ends before the break; the next line starts after it.
            let height = filled_height(line_height, styled, offset, shaper);
            lines.push(Line {
                top,
                height,
                clusters: core::mem::take(&mut clusters),
                bidi_boundaries: None,
                start_byte,
                end_byte: byte,
                end_x: x,
                has_end: true,
                continues_cluster: false,
            });
            top += height;
            x = Fixed::ZERO;
            line_height = Fixed::ZERO;
            continued = false;
            start_byte = byte + cluster.len() as u64;
            end_byte = start_byte;
            continue;
        }
        let style = styled.style_at(offset as u64);
        let advance = shaper.advance(cluster, style);
        if continued && x + advance > width {
            // The carried line is already full: it ends here, in this window, with no clusters of
            // its own and no caret — its own window already offered the caret that ends it.
            let height = filled_height(line_height, styled, offset, shaper);
            lines.push(Line {
                top,
                height,
                clusters: core::mem::take(&mut clusters),
                bidi_boundaries: None,
                start_byte,
                end_byte,
                end_x: x,
                has_end: false,
                continues_cluster: false,
            });
            top += height;
            x = Fixed::ZERO;
            line_height = Fixed::ZERO;
            start_byte = byte;
        }
        line_height = line_height.max(shaped_line_height(shaper, style));
        // A line with a cluster on it starts there, whether the cluster opened the line or was
        // pushed onto it: either way the next cluster must not end it early.
        continued = true;
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
        bidi_boundaries: None,
        start_byte,
        end_byte,
        end_x: x,
        has_end: true,
        continues_cluster: false,
    });
    if let Some(first) = lines.first_mut() {
        first.continues_cluster = opens_inside_a_cluster;
    }
    if let Some(bidi) = WindowBidi::resolve(text) {
        for line in &mut lines {
            if line.clusters.is_empty() {
                continue;
            }
            let rel = usize::try_from(line.start_byte.saturating_sub(origin)).unwrap_or(0);
            if let Some(para) = bidi.paragraph_at(rel) {
                reorder_bidi_line(line, origin, para);
            }
        }
    }
    let total = top + height;
    Layout {
        lines,
        width,
        height: total,
        carry: segments.carry(),
    }
}

/// Reorders one wrapped line's clusters into UAX #9 visual left-to-right order (`L1.4`, `L2`) and
/// pairs every logical cluster boundary bijectively onto the line's visual boundary positions.
fn reorder_bidi_line(line: &mut Line<'_>, origin: u64, para: &ResolvedParagraph) {
    let m = line.clusters.len();
    let base_x = line.clusters[0].x;
    let para_level = para.para_level();
    let mut info: Vec<(BidiClass, u8)> = Vec::with_capacity(m);
    for cluster in &line.clusters {
        let rel = usize::try_from(cluster.byte.saturating_sub(origin)).unwrap_or(0);
        info.push(para.cluster_info(rel, cluster.text.len()));
    }
    let vis_order = reorder_line(para_level, &mut info);
    let mut cursor = base_x;
    let mut placed_x = vec![Fixed::ZERO; m];
    let mut reordered = Vec::with_capacity(m);
    for &logical_idx in &vis_order {
        let mut cluster = line.clusters[logical_idx];
        cluster.x = cursor;
        placed_x[logical_idx] = cursor;
        cursor += cluster.advance;
        reordered.push(cluster);
    }

    let leading = |idx: usize| -> Fixed {
        if info[idx].1 % 2 == 0 {
            placed_x[idx]
        } else {
            placed_x[idx] + line.clusters[idx].advance
        }
    };
    let trailing = |idx: usize| -> Fixed {
        if info[idx].1 % 2 == 0 {
            placed_x[idx] + line.clusters[idx].advance
        } else {
            placed_x[idx]
        }
    };

    let skip = usize::from(line.continues_cluster);
    let mut bounds = Vec::with_capacity(m + 1);
    for k in skip..m {
        let x = if k == 0 {
            if para_level <= info[0].1 {
                if para_level % 2 == 0 {
                    base_x
                } else {
                    line.end_x
                }
            } else {
                leading(0)
            }
        } else if info[k - 1].1 <= info[k].1 {
            trailing(k - 1)
        } else {
            leading(k)
        };
        bounds.push((x, line.clusters[k].byte));
    }
    if line.has_end {
        let end_caret_x = if para_level <= info[m - 1].1 {
            if para_level % 2 == 0 {
                line.end_x
            } else {
                base_x
            }
        } else {
            trailing(m - 1)
        };
        bounds.push((end_caret_x, line.end_byte));
    }
    bounds.sort_by_key(|&(x, byte)| (x, byte));
    line.clusters = reordered;
    line.bidi_boundaries = Some(bounds);
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

    /// The window's bytes, reassembled from the run tree in line order, must equal the window.
    ///
    /// Clusters carry their own text and each line carries its boundaries, so the bytes between
    /// one line's end and the next line's start are exactly the break that separated them —
    /// nothing, for a soft wrap. This is the invariant that catches a break being swallowed into a
    /// cluster: the newline would simply be gone from the reconstruction.
    fn assert_consumes_every_byte(laid: &Layout<'_>, text: &str) {
        let mut rebuilt = String::new();
        for (index, line) in laid.lines().iter().enumerate() {
            for cluster in line.clusters() {
                rebuilt.push_str(cluster.text);
            }
            match laid.lines().get(index + 1) {
                Some(next) => {
                    let start = line.end_byte() as usize;
                    let end = next.start_byte() as usize;
                    assert!(start <= end, "lines overlap in {text:?}");
                    rebuilt.push_str(&text[start..end]);
                }
                None => {
                    let end = line.end_byte();
                    assert_eq!(end, text.len() as u64, "lost the tail of {text:?}");
                }
            }
        }
        assert_eq!(rebuilt, text);
    }

    #[test]
    fn the_run_tree_holds_every_byte_of_the_window_exactly_once() {
        let texts = [
            "",
            "abc",
            "a\n\nb",
            "a\r\nb",
            "a\rb",
            "a\n\u{301}b",
            "line one\nline two\n",
            "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}x\n山田",
        ];
        for text in texts {
            for width in [1, 3, 10] {
                let laid = layout(text, width);
                assert_consumes_every_byte(&laid, text);
            }
        }
    }

    #[test]
    fn every_kind_of_break_splits_a_line() {
        for text in ["a\nb", "a\r\nb", "a\rb"] {
            let laid = layout(text, 10);
            assert_eq!(laid.lines().len(), 2, "{text:?}");
            assert_eq!(laid.lines()[1].start_byte(), text.len() as u64 - 1);
            assert_eq!(laid.lines()[1].clusters()[0].text, "b");
            // The break itself is not a cluster: it is not drawn and cannot hold a caret.
            assert_eq!(laid.lines()[0].clusters().len(), 1);
        }
    }

    #[test]
    fn a_mark_after_a_break_does_not_swallow_it() {
        let laid = layout("a\n\u{301}b", 10);
        assert_eq!(laid.lines().len(), 2);
        assert_eq!(laid.lines()[1].clusters()[0].text, "\u{301}");
        assert_eq!(laid.lines()[1].clusters()[1].text, "b");
        // The caret at the start of the second line is the first byte after the break.
        let caret = laid.paint_point(2).expect("the second line's first byte");
        assert_eq!(caret.line, 1);
        assert_eq!(caret.x, Fixed::ZERO);
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
            assert_consumes_every_byte(&laid, &text);
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

    #[test]
    fn a_zero_carry_is_the_ordinary_layout() {
        let long = "x".repeat(64);
        for text in ["", "a", "one\ntwo\n", "mimi 中 中", long.as_str()] {
            let plain = layout_styled(StyledText::plain(text), 7, Fixed::units(11), &Fake::new());
            let carried = layout_styled_from(
                StyledText::plain(text),
                7,
                Fixed::ZERO,
                Fixed::units(11),
                &Fake::new(),
            );
            assert_eq!(plain, carried, "{text:?}");
        }
    }

    #[test]
    fn a_continuation_window_breaks_where_the_document_breaks() {
        // A paragraph wide enough to wrap, cut at every cluster: the tail has to break in the same
        // places as the whole, because that is the property the shell's measurement streams on.
        let text = "a paragraph of entirely ordinary words, wrapped";
        let width = Fixed::units(12);
        let whole = layout_styled(StyledText::plain(text), 0, width, &Fake::new());
        for cut in 1..text.len() {
            let drawn: Vec<(Fixed, u64)> = whole
                .lines()
                .iter()
                .flat_map(|line| line.clusters().iter())
                .filter(|cluster| cluster.byte >= cut as u64)
                .map(|cluster| (cluster.x, cluster.byte))
                .collect();
            let Some(&(x, start)) = drawn.first() else {
                continue;
            };
            // Only cuts strictly inside a line: a cut at a line start is not a continuation.
            let inside = whole
                .lines()
                .iter()
                .any(|line| line.start_byte() < start && start < line.end_byte());
            if !inside {
                continue;
            }
            let tail = layout_styled_from(
                StyledText::plain(&text[start as usize..]),
                start,
                x,
                width,
                &Fake::new(),
            );
            let first: Vec<(Fixed, u64)> = tail.lines()[0]
                .clusters()
                .iter()
                .map(|cluster| (cluster.x, cluster.byte))
                .collect();
            let containing = whole
                .lines()
                .iter()
                .find(|line| line.start_byte() < start && start < line.end_byte())
                .expect("the cut is inside a line");
            let expected: Vec<(Fixed, u64)> = containing
                .clusters()
                .iter()
                .filter(|cluster| cluster.byte >= start)
                .map(|cluster| (cluster.x, cluster.byte))
                .collect();
            assert_eq!(first, expected, "the first line of the tail at {start}");

            let tail_starts: Vec<u64> = tail
                .lines()
                .iter()
                .skip(1)
                .map(|line| line.start_byte())
                .collect();
            let whole_starts: Vec<u64> = whole
                .lines()
                .iter()
                .map(|line| line.start_byte())
                .filter(|byte| *byte > start)
                .collect();
            assert_eq!(
                tail_starts, whole_starts,
                "the lines after the tail at {start}"
            );
        }
    }

    #[test]
    fn a_window_that_opens_inside_a_cluster_offers_no_boundary_there() {
        // `e` + U+0301 is one cluster, so the document's carets are 0, 3 and 4. A window over the
        // mark's bytes, handed the state that says the cluster is open, must offer 3 and 4 only.
        let mut prefix = Clusters::new("e");
        while prefix.next().is_some() {}
        let width = Fixed::units(200);
        let continued = layout_styled_continuing(
            StyledText::plain("\u{301}x"),
            1,
            Fixed::ZERO,
            prefix.carry(),
            width,
            &Fake::new(),
        );
        let offered: Vec<u64> = continued.lines()[0]
            .boundaries()
            .map(|(_, byte)| byte)
            .collect();
        assert_eq!(offered, vec![3, 4], "the mark's own byte is not a caret");
        assert!(continued.paint_point(1).is_none());
        assert!(continued.paint_point(3).is_some());

        // Told nothing about the text before it, the same bytes do offer byte 1 — which is the
        // caret the document does not have, and the whole reason the state is carried.
        let alone = layout_styled(StyledText::plain("\u{301}x"), 1, width, &Fake::new());
        assert!(alone.paint_point(1).is_some());
    }

    #[test]
    fn a_carried_line_that_is_full_wraps_before_its_first_cluster() {
        let width = Fixed::units(10);
        let laid = layout_styled_from(StyledText::plain("abc"), 40, width, width, &Fake::new());
        assert_eq!(
            laid.lines().len(),
            2,
            "the full line, then the cluster's line"
        );
        let carried = &laid.lines()[0];
        assert!(
            carried.clusters().is_empty(),
            "the tail has no clusters here"
        );
        assert!(!carried.has_end(), "a soft wrap offers no caret of its own");
        assert_eq!(carried.end_x(), width);
        assert_eq!(carried.start_byte(), 40, "the line it inherits ends here");
        let fresh = &laid.lines()[1];
        assert_eq!(fresh.start_byte(), 40);
        assert_eq!(
            fresh.clusters()[0].x,
            Fixed::ZERO,
            "the cluster starts the line"
        );
    }
}
