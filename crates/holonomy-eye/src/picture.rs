//! The frame as a picture: the owned view model a renderer draws (ROADMAP Block 5, `docs/TO-APP.md`
//! M1).
//!
//! [`Layout`] is the geometry, but it borrows the window's text and hands a renderer `Placed`
//! clusters with styles and byte offsets interleaved with the layout that produced them. A
//! renderer does not want to walk a layout: it wants a flat, owned, drawable list — for each line
//! its box and where its clusters sit, for each cluster a byte and an advance, plus the two things
//! a layout cannot answer by itself, the caret and the selection.
//!
//! Two properties are what make this a *model* rather than a convenience:
//!
//! - **A picture is a picture.** Every line has a box, boxes tile the content top to bottom, and
//!   every cluster's cell is claimed once and by one cluster. `holonomy-shell`'s raster harness is
//!   the machine check: it paints a picture into a grid of one-unit cells and asserts that the
//!   grid is exactly as full as the picture says it is.
//! - **Selection is geometry.** A selection is not a pair of byte offsets a renderer re-derives
//!   per frame; it is runs of a line, at x coordinates the same walk that places carets produced.
//!   Partial clusters paint whole, because a cluster is atomic — every editor does this, and here
//!   the rule is stated once, in [`Layout::selection_spans`], instead of in every renderer.

use crate::geom::Fixed;
use crate::layout::{Caret, Layout};
use crate::style::Style;

/// One horizontal run of a line that a selection paints.
///
/// `line` indexes [`Picture::lines`]. `x..end` are in the layout's logical units from the window's
/// left margin, so a span never needs a second transform to be drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionSpan {
    /// Index of the line the run is on.
    pub line: usize,
    /// Left edge of the run.
    pub x: Fixed,
    /// Right edge of the run.
    pub end: Fixed,
}

/// One cluster as a renderer takes it: a byte, where it sits, and how wide it is.
///
/// The text itself is not copied — the renderer holds the window's text (from the same frame the
/// picture came from) and slices it at `byte` for `len` bytes. That keeps a picture a few hundred
/// integers instead of a copy of the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PictureCluster {
    /// Document byte offset of the cluster's first byte.
    pub byte: u64,
    /// Left edge, in logical units.
    pub x: Fixed,
    /// Advance width, in logical units.
    pub advance: Fixed,
    /// Length of the cluster in bytes.
    pub len: usize,
    /// The style the cluster was shaped with.
    pub style: Style,
}

impl PictureCluster {
    /// The right edge of the cluster.
    #[must_use]
    pub fn end(&self) -> Fixed {
        self.x + self.advance
    }
}

/// One line's box and the range of [`Picture::clusters`] it owns.
///
/// Clone but not `Copy`: the range into [`Picture::clusters`] is not a copyable value, and a
/// renderer that keeps a line keeps its cluster range with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PictureLine {
    /// Top of the line box, in logical units from the window's top.
    pub top: Fixed,
    /// Height of the line box.
    pub height: Fixed,
    /// Document byte where the line's content starts.
    pub start_byte: u64,
    /// Document byte where the line's content ends — before a hard break's newline.
    pub end_byte: u64,
    /// Where the line's content ends, horizontally.
    pub end_x: Fixed,
    /// Whether the line offers a caret at its end.
    pub has_end: bool,
    /// Range of [`Picture::clusters`] placed on this line, left to right.
    pub clusters: core::ops::Range<usize>,
}

/// A frame, ready to draw.
///
/// Everything here is owned and `Copy`-by-value: a renderer can drop the frame, keep the picture,
/// and draw it on a paint callback that outlives the borrow. Nothing in it needs a shape, a style
/// lookup or a hit test to be answered — those all happened before it existed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Picture {
    /// The lines, top to bottom.
    pub lines: Vec<PictureLine>,
    /// Every line's clusters, in line order.
    pub clusters: Vec<PictureCluster>,
    /// Where the caret is, when it is in this picture.
    pub caret: Option<Caret>,
    /// The selection's runs, in line order.
    pub selection: Vec<SelectionSpan>,
    /// The width the text was wrapped to.
    pub width: Fixed,
    /// The total height of the lines.
    pub height: Fixed,
}

impl Picture {
    /// The clusters on line `index`, left to right.
    #[must_use]
    pub fn clusters_on(&self, index: usize) -> &[PictureCluster] {
        self.lines
            .get(index)
            .and_then(|line| self.clusters.get(line.clusters.clone()))
            .unwrap_or(&[])
    }

    /// The selection run on line `index`, if any.
    #[must_use]
    pub fn selection_on(&self, index: usize) -> Option<SelectionSpan> {
        self.selection
            .iter()
            .copied()
            .find(|span| span.line == index)
    }
}

impl<'a> Layout<'a> {
    /// The picture of this layout: its lines and clusters, plus the caret at `caret` and the
    /// selection between `selection`'s ends.
    ///
    /// Both extras are optional and are simply absent when they are not in this window: a caret
    /// scrolled off the screen is not drawn, and a selection that does not touch the window paints
    /// nothing. Nothing here recomputes geometry — a cluster's x and advance are the layout's, and
    /// a caret's are [`Layout::paint_point`]'s.
    #[must_use]
    pub fn picture(&self, caret: Option<u64>, selection: Option<(u64, u64)>) -> Picture {
        let mut clusters = Vec::new();
        let mut lines = Vec::with_capacity(self.lines().len());
        for line in self.lines() {
            let from = clusters.len();
            for cluster in line.clusters() {
                clusters.push(PictureCluster {
                    byte: cluster.byte,
                    x: cluster.x,
                    advance: cluster.advance,
                    len: cluster.text.len(),
                    style: cluster.style,
                });
            }
            lines.push(PictureLine {
                top: line.top(),
                height: line.height(),
                start_byte: line.start_byte(),
                end_byte: line.end_byte(),
                end_x: line.end_x(),
                has_end: line.has_end(),
                clusters: from..clusters.len(),
            });
        }
        Picture {
            lines,
            clusters,
            caret: caret.and_then(|byte| self.paint_point(byte)),
            selection: selection
                .map_or_else(Vec::new, |(start, end)| self.selection_spans(start, end)),
            width: self.width(),
            height: self.height(),
        }
    }

    /// The runs of each line a selection from `start` to `end` paints, in line order.
    ///
    /// The ends may be in either order. A selection paints from a cluster's left edge to a
    /// cluster's right edge: a range that ends *inside* a cluster paints the whole cluster, because
    /// a cluster is atomic and a half-painted grapheme is not a picture of anything. A range that
    /// covers a hard break paints to the end of the line's content on the line before it and from
    /// the start of the next line on the line after; the break itself is painted by neither.
    #[must_use]
    pub fn selection_spans(&self, start: u64, end: u64) -> Vec<SelectionSpan> {
        let (start, end) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        if start == end {
            return Vec::new();
        }
        let mut spans = Vec::new();
        for (index, line) in self.lines().iter().enumerate() {
            let from = start.max(line.start_byte());
            let to = end.min(line.end_byte());
            if from >= to {
                continue;
            }
            let x = line.x_at(from);
            let end_x = line.x_after(to);
            if end_x > x {
                spans.push(SelectionSpan {
                    line: index,
                    x,
                    end: end_x,
                });
            }
        }
        spans
    }
}
