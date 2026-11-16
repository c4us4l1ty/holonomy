# RFC-004 — The Eye (Block 5)

*Status: open attack target. File attack cases in `AUDIT.md`.*

## Context

v3's death certificate is the two-geometry disease (AUDIT wall K-8): glyph runs painted with
true advances; caret/selection computed in `'m'`-cell units. Seventeen failing tests shipped
red on main, and the lesson generalizes: **any duplicated geometry drifts**. v5's solution to
this problem was to avoid having an editor at all — an option v6 does not have. So the Eye
is specified as a standalone crate with its constitutional test before the UI exists.

## Decision

1. **One run tree**: layout consumes Spine text+styles for the viewport slab window and
   produces `ShapedRun` trees. Paint walks them; hit-test walks them; selection walks them.
   There is one arithmetic and it is proportional — a monospace *rendering choice* is a style,
   not a second engine.
2. **Viewport-only shaping**: shaping cost is invariant in document size because input is
   invariant in document size (the Spine guarantees resident input is viewport-windowed).
3. **Pagination is layout, not text**: page boundaries derive from the Fenwick height model
   of slabs — never document content (K-9's newline-padding heresy is impossible here because
   the Eye has no write access to the Spine).
4. **Cluster-honest carets**: caret positions are grapheme-cluster boundaries (BiDi-aware,
   emoji-safe); `hit_test` rounds clicks to clusters. L-8's property test covers this.
5. **Zoom/DPI as pure view transform**: the run tree is logical-unit; the paint layer scales.
   No re-shape per zoom tick beyond font-metric nontrivial cases, and **both consumers share
   the transform**, so zoom cannot open a paint/click seam.

## Attacks resisted

| Failure | Mechanism | Control | Proving test |
| ------- | --------- | ------- | ------------ |
| K-8 drift | two arithmetics | one run tree | L-8: `hit_test(paint_point(g)) == g` ∀ glyph, zoom, DPI |
| perf blowup | shaping the corpus | windowed input only | view-invariance timing sweep |
| caret lies | byte-position carets vs. glyphs | cluster boundaries | bidi/emoji corpus hit-tests |
| font-metric ache | atlas ceilings (H1's 26 ppem cap) | GPU text pipeline via GPUI; logical metrics | heading-scale golden shots |

## Rejected alternatives

- **Cell-grid terminal-style geometry** (H1's original comfort): dead with the proportional
  UI spec; kept alive in the codebase precisely nowhere.
- **Full-document preemptive layout cache**: memory blowup disguised as speed (A2-relevant);
  rejected. Cache = viewport window + Fenwick skeleton only.

## Open questions

- Math/table/image boxes (deferred to post-6.0 composition surfaces) need a second layout
  dimension; the run tree is specified with block-level extension points now so the future
  addition cannot fork the geometry: one tree, new node kinds.
