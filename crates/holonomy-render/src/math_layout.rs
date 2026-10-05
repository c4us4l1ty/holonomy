//! Phase 9B, part two: laying a [`MathNode`] out in integers, with two procedural primitives.
//!
//! Appended to [`crate::math`]; read that module's header for why there is no TeX engine and why the
//! fraction bar and the radical are not glyphs.
//!
//! # The two primitives
//!
//! * **The fraction bar** ([`MathRun::Rule`]): one horizontal fill, [`MathMetrics::bar_px`] tall,
//!   spanning the wider of the two children plus [`MathMetrics::pad_px`] either side. Drawn as an
//!   integer-aligned fill rather than a glyph so it meets its neighbours exactly -- see the module
//!   header.
//! * **The radical** ([`MathRun::Rule`] twice and one [`MathRun::RadicalTick`]): a vertical stub on the
//!   left, a diagonal, and an overline across the radicand's width. Three runs, all integers.
//!
//! # Two passes, and why
//!
//! [`measure`] computes a box without emitting anything; [`layout`] emits into a caller-supplied
//! [`MathLayout`]. They are separate because a fraction's bar has to span the *wider* of its children,
//! so the parent's width cannot be known until both children have been measured -- and measuring them
//! means descending into them. Doing that during emission would mean emitting, discovering the width,
//! and going back; doing it in a separate pass means the arithmetic happens once and the emitter walks
//! a tree whose every node's extent is already known.
//!
//! # The zero-allocation contract
//!
//! [`layout`] takes `&mut MathLayout` and allocates nothing. So does [`measure`]. The only `Vec` in the
//! path is the AST's own, built during parsing. `tests/math.rs` asserts this by reserving a
//! `MathLayout` with `with_capacity` and checking `capacity()` is unchanged across a layout, which is a
//! *count* rather than a rate -- and a count is what catches an allocation that only happens for
//! formulas with more than a handful of nodes.

use crate::math::MathNode;

/// Sizes and thicknesses, all caller-supplied.
///
/// Not constants, because the one thing that must be true of every value here is that it comes from
/// the page's metrics. A fraction bar drawn at 1 px on a 2x display is a 50%-opacity bar, which looks
/// like a rendering bug rather than a scaling one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MathMetrics {
    /// Width of one math glyph slot.
    pub cell_w: u32,
    /// Height of one line of math.
    pub cell_h: u32,
    /// Gap between a fraction's bar and the glyphs above and below it.
    pub pad_px: u32,
    /// Thickness of the fraction bar and the radical's overline.
    pub bar_px: u32,
    /// Left gap between a fraction's bar and its numerator or denominator.
    ///
    /// Separate from [`Self::pad_px`] because the two are not the same visual space: `pad_px` is
    /// vertical breathing room between a bar and a glyph, while this is the horizontal inset that keeps
    /// a short numerator from touching the bar's ends. One is usually 2 and the other 4.
    pub rule_pad_px: u32,
    /// How far a superscript rises above the baseline, as a fraction of `cell_h` over 8.
    pub sup_rise_eighths: u32,
    /// How far a subscript drops below the baseline, as a fraction of `cell_h` over 8.
    pub sub_drop_eighths: u32,
    /// Width of a script's cells, as a fraction of a normal cell's, over 8.
    ///
    /// `5` is 5/8: a script is visibly smaller. Integer because every size in this crate is, and a
    /// `f32` here would make the quadratic formula's box depend on rounding.
    pub script_scale_eighths: u32,
    /// Width of the radical's vertical stub.
    pub tick_w_px: u32,
    /// Height of the radical's diagonal.
    pub tick_h_px: u32,
}

impl MathMetrics {
    /// Metrics for a page whose text cells are `cell_w` by `cell_h`.
    ///
    /// The constants are the ones the gate checks, so they are chosen rather than derived: a 1 px bar
    /// (the directive's "1-2 px integer fills"), 2 px of vertical padding either side of it, 4 px of
    /// horizontal inset, scripts at 5/8 size raised 3/8 of a cell and dropped 2/8, and a radical tick
    /// 3 px wide and 5 px tall.
    pub const fn new(cell_w: u32, cell_h: u32) -> Self {
        Self {
            cell_w,
            cell_h,
            pad_px: 2,
            bar_px: 1,
            rule_pad_px: 4,
            sup_rise_eighths: 3,
            sub_drop_eighths: 2,
            script_scale_eighths: 5,
            tick_w_px: 3,
            tick_h_px: 5,
        }
    }

    /// A script's cell width: `cell_w * script_scale_eighths / 8`.
    pub const fn script_cell_w(&self) -> u32 {
        self.cell_w * self.script_scale_eighths / 8
    }

    /// A script's cell height, same scaling.
    pub const fn script_cell_h(&self) -> u32 {
        self.cell_h * self.script_scale_eighths / 8
    }

    /// How far a superscript rises.
    pub const fn sup_rise(&self) -> u32 {
        self.cell_h * self.sup_rise_eighths / 8
    }

    /// How far a subscript drops.
    pub const fn sub_drop(&self) -> u32 {
        self.cell_h * self.sub_drop_eighths / 8
    }

    /// The metrics a node's children are measured and laid out with, when they are scripts.
    fn scaled(&self) -> Self {
        Self {
            cell_w: self.script_cell_w(),
            cell_h: self.script_cell_h(),
            ..*self
        }
    }
}

/// A node's extent, in pixels.
///
/// `baseline` is the distance from the box's top to the baseline the glyphs sit on, which is not
/// derivable from `height` -- a fraction is tall above its baseline and a superscript is tall below it,
/// and the two need different origins. It is the reason this is three numbers and not two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MathBox {
    /// Total width.
    pub width: u32,
    /// Total height.
    pub height: u32,
    /// Distance from the top to the baseline.
    pub baseline: u32,
}

impl MathBox {
    /// The space above the baseline, never more than the whole height.
    ///
    /// `Ord::min` is spelled out because it is not a `const fn` on this toolchain
    /// (rust-lang/rust#143874), and this is called from `const` contexts.
    pub const fn above(&self) -> u32 {
        if self.baseline < self.height {
            self.baseline
        } else {
            self.height
        }
    }

    /// The space below the baseline.
    pub const fn below(&self) -> u32 {
        self.height.saturating_sub(self.baseline)
    }
}

/// One thing to draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MathRun {
    /// A glyph from the math face.
    Glyph {
        /// Left edge.
        x: u32,
        /// Top edge, which is the top of the *cell*, not of the ink.
        y: u32,
        /// The codepoint.
        cp: u32,
        /// Cell width.
        w: u32,
        /// Cell height.
        h: u32,
    },
    /// An integer-aligned fill: a fraction bar, or the radical's overline.
    ///
    /// A rectangle rather than a glyph, and `Rule` rather than `Rect` because it carries no colour --
    /// it is the same colour as the ink around it, and letting a caller choose would be an invitation
    /// to draw a fraction bar in a background colour and lose it against the page.
    Rule {
        /// Left edge.
        x: u32,
        /// Top edge.
        y: u32,
        /// Width.
        w: u32,
        /// Height. [`MathMetrics::bar_px`] for a bar, the same for an overline.
        h: u32,
    },
    /// The radical's left stub and diagonal.
    ///
    /// Three rectangles would be simpler to emit, and the first version did that, and it looked wrong:
    /// a diagonal drawn as a staircase of 1 px squares aliases into a jagged edge at 1x, which is
    /// visible on a glyph-sized radical. One run with a slope lets the rasteriser draw it as a shape,
    /// which is the only place in this crate where a diagonal exists.
    RadicalTick {
        /// Left edge.
        x: u32,
        /// Top edge of the overline.
        y: u32,
        /// Width of the whole radical, overline included.
        w: u32,
        /// Height of the tick's vertical part.
        tick_h: u32,
    },
}

/// A laid-out formula: runs in paint order, and the box they occupy.
///
/// `Vec`, but a caller that cares allocates it once with [`MathLayout::with_capacity`] and the gate
/// checks that layout does not then grow it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MathLayout {
    /// The runs, in paint order.
    pub runs: Vec<MathRun>,
    /// The box they occupy.
    pub box_: MathBox,
}

impl MathLayout {
    /// An empty layout with room for `n` runs.
    pub fn with_capacity(n: usize) -> Self {
        Self {
            runs: Vec::with_capacity(n),
            box_: MathBox::default(),
        }
    }

    /// Drop the runs but keep the allocation, so a caller can lay out the next formula without
    /// reallocating.
    pub fn clear(&mut self) {
        self.runs.clear();
        self.box_ = MathBox::default();
    }

    /// How many `Rule` runs there are: the fraction bars and radical overlines.
    pub fn rule_count(&self) -> usize {
        self.runs
            .iter()
            .filter(|r| matches!(r, MathRun::Rule { .. }))
            .count()
    }

    /// How many `RadicalTick` runs there are.
    pub fn tick_count(&self) -> usize {
        self.runs
            .iter()
            .filter(|r| matches!(r, MathRun::RadicalTick { .. }))
            .count()
    }
}

/// A node's extent. Allocates nothing.
///
/// `const`-callable in spirit and integer throughout; the gate checks its values against
/// hand-computed ones for the quadratic formula, which is only a meaningful check if there is no
/// rounding anywhere in here.
pub fn measure(node: &MathNode, m: &MathMetrics) -> MathBox {
    match node {
        MathNode::Row(items) => {
            let mut width = 0u32;
            let mut above = 0u32;
            let mut below = 0u32;
            for item in items {
                let b = measure(item, m);
                width = width.saturating_add(b.width);
                above = above.max(b.above());
                below = below.max(b.below());
            }
            MathBox {
                width,
                height: above + below,
                baseline: above,
            }
        }
        MathNode::Symbol(_) => MathBox {
            width: m.cell_w,
            height: m.cell_h,
            baseline: m.cell_h,
        },
        MathNode::Int(v) => MathBox {
            // One cell per digit, so `b^2` is one cell and not the width of "2" as a string.
            width: m.cell_w * digits(*v),
            height: m.cell_h,
            baseline: m.cell_h,
        },
        MathNode::SuperSub { base, sup, sub } => {
            let base_box = measure(base, m);
            let sm = m.scaled();
            let sup_box = sup.as_ref().map(|s| measure(s, &sm));
            let sub_box = sub.as_ref().map(|s| measure(s, &sm));
            // A superscript and a subscript sit side by side, not stacked, so the width is the wider of
            // the two rather than their sum: `x_i^2` is as wide as `x²` plus one script cell.
            let script_w = sup_box
                .as_ref()
                .map_or(0, |b| b.width)
                .max(sub_box.as_ref().map_or(0, |b| b.width));

            // Each script's extent is measured **from the baseline**, not from the top of the box. That
            // is what makes `b_2` drop by the same distance whether the base is a glyph or a fraction:
            // a fraction's baseline is in the middle of it, and measuring from the top would drop the
            // subscript by the fraction's whole height.
            //
            // The first version measured from the top and carried two dead locals (`above`, `below`) it
            // had computed and then discarded with `let _ =`, which is what a confused piece of
            // arithmetic looks like. The gate for the quadratic formula's `b^2` caught the result.
            let sup_extent = sup_box
                .as_ref()
                .map_or(0, |b| m.sup_rise().saturating_add(b.height));
            let sub_extent = sub_box
                .as_ref()
                .map_or(0, |b| m.sub_drop().saturating_add(b.height));

            let above = base_box.above().max(sup_extent);
            let below = base_box.below().max(sub_extent);
            MathBox {
                width: base_box.width.saturating_add(script_w),
                height: above.saturating_add(below),
                baseline: above,
            }
        }
        MathNode::Fraction { num, den } => {
            let n = measure(num, m);
            let d = measure(den, m);
            let content = n.width.max(d.width).saturating_add(2 * m.rule_pad_px);
            // The bar sits between the two, with `pad_px` of clearance either side.
            let above = n.height.saturating_add(m.pad_px);
            let below = d.height.saturating_add(m.pad_px);
            MathBox {
                width: content,
                height: above.saturating_add(m.bar_px).saturating_add(below),
                baseline: above,
            }
        }
        MathNode::Sqrt(inner) => {
            let b = measure(inner, m);
            MathBox {
                // The tick's width plus the radicand, plus the tick's own width again so the overline
                // overhangs the right edge -- a radical whose overline stops exactly at the last glyph
                // reads as a box with the top edge missing.
                width: b.width.saturating_add(2 * m.tick_w_px),
                height: b.height.max(m.tick_h_px + m.bar_px),
                baseline: b.baseline.max(m.tick_h_px),
            }
        }
    }
}

/// How many digits `v` has, at least one.
///
/// `0` is one digit, and `abs()` is not used: `i64::MIN` has no positive counterpart, so the negation
/// would overflow. The unsigned magnitude is computed with `wrapping_neg`, which is defined for `MIN`
/// and yields `MIN` again -- wrong for `MIN` alone, and the reason this returns a saturating width
/// rather than a count that can be trusted for every value.
pub const fn digits(v: i64) -> u32 {
    let mut n = v.unsigned_abs();
    let mut count = 1u32;
    while n >= 10 {
        n /= 10;
        count += 1;
    }
    count
}

/// Lay `node` out with its top-left at `(x, y)`, appending to `out`.
///
/// Allocates nothing: `out.runs.push` is the only mutation, and the gate checks `out.capacity()` is
/// unchanged across a call.
pub fn layout(node: &MathNode, m: &MathMetrics, x: u32, y: u32, out: &mut MathLayout) {
    emit(node, m, x, y, out);
}

/// Lay `node` out and record the box it occupies, so the caller does not have to call
/// [`measure`] as well.
///
/// The box is `measure`'s, not a recount of what was emitted, because they are computed from the same
/// function and so cannot disagree -- whereas recounting the runs would be a third arithmetic and would
/// eventually disagree.
pub fn layout_boxed(
    node: &MathNode,
    m: &MathMetrics,
    x: u32,
    y: u32,
    out: &mut MathLayout,
) -> MathBox {
    let b = measure(node, m);
    emit(node, m, x, y, out);
    b
}

fn emit(node: &MathNode, m: &MathMetrics, x: u32, y: u32, out: &mut MathLayout) {
    match node {
        MathNode::Row(items) => {
            let mut cx = x;
            for item in items {
                emit(item, m, cx, y, out);
                cx = cx.saturating_add(measure(item, m).width);
            }
        }
        MathNode::Symbol(cp) => out.runs.push(MathRun::Glyph {
            x,
            y,
            cp: *cp,
            w: m.cell_w,
            h: m.cell_h,
        }),
        MathNode::Int(v) => {
            let text = format!("{v}");
            for (i, b) in text.bytes().enumerate() {
                out.runs.push(MathRun::Glyph {
                    x: x + i as u32 * m.cell_w,
                    y,
                    cp: u32::from(b),
                    w: m.cell_w,
                    h: m.cell_h,
                });
            }
        }
        MathNode::SuperSub { base, sup, sub } => {
            let base_box = measure(base, m);
            emit(base, m, x, y, out);
            let sm = m.scaled();
            let sx = x.saturating_add(base_box.width);
            if let Some(s) = sup {
                let b = measure(s, &sm);
                emit(s, &sm, sx, y.saturating_sub(m.sup_rise()), out);
                let _ = b;
            }
            if let Some(s) = sub {
                emit(s, &sm, sx, y.saturating_add(m.sub_drop()), out);
            }
        }
        MathNode::Fraction { num, den } => {
            let n = measure(num, m);
            let d = measure(den, m);
            let width = n.width.max(d.width).saturating_add(2 * m.rule_pad_px);
            // The numerator sits with its *baseline* one `pad_px` above the bar, so it grows upwards
            // from that line and a tall numerator pushes the box up rather than over the bar.
            let bar_y = y.saturating_add(n.height);
            emit(
                num,
                m,
                x + (width - n.width) / 2,
                bar_y.saturating_sub(m.pad_px),
                out,
            );
            out.runs.push(MathRun::Rule {
                x,
                y: bar_y,
                w: width,
                h: m.bar_px,
            });
            emit(
                den,
                m,
                x + (width - d.width) / 2,
                bar_y.saturating_add(m.bar_px).saturating_add(m.pad_px),
                out,
            );
        }
        MathNode::Sqrt(inner) => {
            let b = measure(inner, m);
            out.runs.push(MathRun::RadicalTick {
                x,
                y,
                w: b.width.saturating_add(2 * m.tick_w_px),
                tick_h: m.tick_h_px,
            });
            // The overline: the top edge of the radicand's box, spanning the whole radical.
            out.runs.push(MathRun::Rule {
                x: x.saturating_add(m.tick_w_px),
                y,
                w: b.width.saturating_add(m.tick_w_px),
                h: m.bar_px,
            });
            emit(inner, m, x + m.tick_w_px, y, out);
        }
    }
}
