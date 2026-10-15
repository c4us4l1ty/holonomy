//! **Which atlas sizes to build for a `--ppem`, and what happened instead.** Part 46.
//!
//! # Why this is a module and not four lines in `main`
//!
//! **Because the claim this makes is testable and the place it lived was not.** Part 43 put
//! `atlas_sizes` in `main.rs` as a private function and verified it exactly once — at `[16, 20]`,
//! which fits, 378,850 bytes of 458,752. **Every other `--ppem` was assumed to behave the same way
//! because it was computed by the same expression**, and it does not: at the default `--ppem 20`
//! the same expression produces glyphs that need 554,236 bytes and the application aborted with
//!
//! ```text
//! the packed faces must rasterise: AtlasFull { need: 554236, have: 458752 }
//! ```
//!
//! **A function that decides something and cannot be asked about its decision is a function that
//! will be wrong silently.** So the decision lives here, it returns what it did rather than
//! printing it, and [`tests/atlas_plan.rs`](../../tests/atlas_plan.rs) holds it.
//!
//! # The measured law
//!
//! Bytes of glyph coverage against a 458,752-byte atlas, by actually rasterising every pair:
//!
//! | `--ppem` | one size | two sizes, `(base, base*5/4)` |
//! |---|---|---|
//! | 10 | 70,478 | 165,640 |
//! | 16 | 153,571 | **378,850** |
//! | 17 | 169,174 | **412,608** |
//! | 18 | 186,916 | 450,645 — **does not fit** |
//! | 20 | 225,279 | 554,236 — **does not fit** |
//! | 24 | 308,864 | 767,350 — **does not fit** |
//! | 26 | 353,961 | 865,205 — **does not fit** |
//!
//! **Two sizes fit up to `--ppem 17` and not one step further.** The coverage grows with the *area*
//! of the glyphs, so a second size nearly doubles it, and `base + second <= 38` is about where the
//! wall is — **but that is a coincidence of today's codepoint ranges, not a law, and it is
//! deliberately not written down as a constant here.** A formula that encodes the glyph set's area
//! is the same class of bug one range change away from, and it fails by aborting a window somebody
//! was trying to open.
//!
//! # Why try-and-fall-back and not a formula
//!
//! **Because a formula is a second thing to keep in step with the glyph set, and this file is
//! already the one place the sizes are decided.** [`sizes_for`] returns the *wish*; [`build`]
//! finds out whether the budget grants it, and if not, builds one size and returns why.
//!
//! **That costs a second rasterisation pass on the fallback path** — about a tenth of a second,
//! once, at startup — and the alternative is a core dump. **It is not on a keystroke and there is
//! no keystroke budget here to spend.**

use holonomy_assets::atlas::Atlas;

/// **The ppem sizes to *ask* for, for a `--ppem` of `base`: `base` and 125 % of it.**
///
/// # Why 125 %
///
/// **Because `menus::ZOOMS` has two rows, 100 % and 125 %, and the second is the one that needs
/// glyphs rasterised at a size that is not the one the chrome was laid out for.** §2.9.5 forbids
/// rasterising at runtime — it is curve evaluation at runtime — so both sizes are built at boot and
/// *selected* at runtime.
///
/// `sort_unstable` and `dedup`, and the sort is not cosmetic. [`zoom_size_index`] resolves a
/// percentage to a *rank* in `ZOOMS` and uses that rank as an index into [`Atlas::sizes`], so the
/// atlas must hold them ascending or 125 % would draw at 100 %.
///
/// **Two is the ceiling.** `AtlasBuilder::new` refuses a third, because each extra size adds
/// 14,080 bytes of metric table against a 512 KiB budget.
///
/// [`zoom_size_index`]: crate::session::Session::set_zoom
#[must_use]
pub fn sizes_for(base: u16) -> Vec<u16> {
    let mut sizes = vec![base, base.saturating_mul(5) / 4];
    sizes.sort_unstable();
    sizes.dedup();
    sizes
}

/// **What was actually built, and what it cost to find out.**
///
/// **A struct and not a `(Atlas, bool)`.** The bool was the first version and it could not answer
/// *"why"*, which is the only question a user has when the zoom control turns out not to work.
/// Every number here exists so the refusal can name its own cause.
#[derive(Debug)]
pub struct Plan {
    /// The atlas, at one or two sizes.
    pub atlas: Atlas,
    /// Whether the **second** size is present, which is what zoom needs.
    ///
    /// **False means 125 % is unavailable**, and `Session::set_zoom` answers `ZoomUnavailable` —
    /// typed, not a silent clamp.
    pub has_zoom_size: bool,
    /// Bytes of glyphs the **two-size** request needed.
    ///
    /// **Zero when the pair was never asked for**, which happens when `base * 5 / 4 == base`.
    pub refused_need: usize,
    /// The atlas's capacity, for the ratio in a message.
    pub refused_have: usize,
}

impl Plan {
    /// **One line saying why zoom is unavailable, or `None` when it is.**
    ///
    /// **`None` rather than an empty string**, so the caller writes `if let Some(why) = …` and a
    /// caller that forgets gets a `None` it has to deal with rather than a blank line it prints.
    ///
    /// **It names the flag that would fix it.** A refusal the user cannot act on is the same defect
    /// as the keystroke that does nothing.
    #[must_use]
    pub fn zoom_refusal(&self, ppem: u16) -> Option<String> {
        (!self.has_zoom_size).then(|| {
            format!(
                "two atlas sizes need {} bytes of glyphs and the atlas holds {}, so this build has \
                 one size and zoom has nothing to switch to; --ppem 17 or smaller keeps 125 %",
                self.refused_need, self.refused_have
            )
        })
        .map(|m| format!("{m} (at --ppem {ppem})"))
    }
}

/// **Build the atlas at the largest of [`sizes_for`] that the budget allows, and say what happened.**
///
/// # The fallback keeps `base` and loses the second size
///
/// **Because `base` is what the chrome was laid out for.** Every band height, the cell width and the
/// ruler's column count derive from `--ppem`, so drawing the base size from a smaller one would put
/// the glyphs in a chrome built for larger ones. **The second size is the one that can be lost**,
/// and losing it costs exactly one feature.
///
/// # One size always fits, so the second error is a panic and not a `Result`
///
/// **`--ppem` is clamped to 10..=26 in `args.rs`, and the largest of those needs 353,961 bytes of a
/// 458,752-byte atlas.** That is a fact about the clamp and about the glyph set, and
/// `one_size_always_fits_at_every_legal_ppem` is the gate that holds it — so if a future codepoint
/// range ever breaks it, that gate fails here rather than the application aborting on a window.
pub fn build(base: u16) -> Plan {
    let sizes = sizes_for(base);
    match holonomy_assets::build_atlas(&sizes) {
        Ok((atlas, _)) => Plan {
            atlas,
            has_zoom_size: sizes.len() > 1,
            refused_need: 0,
            refused_have: holonomy_assets::metric::ATLAS_BYTES,
        },
        Err(holonomy_assets::Error::AtlasFull { need, have }) if sizes.len() > 1 => {
            let (atlas, _) = holonomy_assets::build_atlas(&[base]).expect(
                "**one size always fits: --ppem is clamped to 10..=26 and all of them fit alone**",
            );
            Plan {
                atlas,
                has_zoom_size: false,
                refused_need: need,
                refused_have: have,
            }
        }
        Err(e) => panic!("the packed faces must rasterise: {e}"),
    }
}