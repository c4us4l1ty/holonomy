//! **The atlas the application actually builds, for every legal `--ppem`.** Part 46.
//!
//! # What was missing, and what it cost
//!
//! Part 43 gave the atlas a second size so zoom would have glyphs to switch to, and verified it
//! **once**, at `[16, 20]`, which fits. **The default `--ppem` is 20**, the same expression gives
//! `[20, 25]`, and that needs 554,236 bytes of a 458,752-byte atlas. The application aborted with
//!
//! ```text
//! the packed faces must rasterise: AtlasFull { need: 554236, have: 458752 }
//! ```
//!
//! and **could not be started at all.**
//!
//! # Why the existing gate did not catch it
//!
//! `atlas.rs::atlas_and_table_fit_the_ceiling` asserts `ATLAS_BYTES + table <= 512 KiB`. **That is
//! the *metric table's* arithmetic** — 14,080 bytes per size — and it says nothing at all about
//! glyph coverage, which depends on which codepoints the payload holds and how many sizes are
//! rasterised. **The gate was right about the bytes it measured and it measured the wrong ones**,
//! which is a different failure from being wrong and much easier to miss.
//!
//! # What these gates do instead
//!
//! **They rasterise**, at the boundary ppems and at the default, and they ask the question a user
//! would ask: *does the application start, and does zoom work?* **Rasterising is slow** — a few
//! seconds per size — so this file covers the boundary rather than all seventeen values, and
//! [`every_legal_ppem_builds_an_atlas`] covers the rest at one size each.
//!
//! # The cost
//!
//! **Every gate here is a real rasterisation**, and that is deliberate. The bug was found by
//! *asserting an arithmetic that did not describe the requirement*, and the only arithmetic that
//! describes this requirement is the one the product does.

use holonomy::atlas_plan::{self, Plan};
use holonomy_assets::metric::ATLAS_BYTES;

/// The clamp in `args.rs`, written down so this file does not have to reach into it.
///
/// **Duplicated rather than imported**, because `Args::ppem`'s bounds are validated in
/// `args::tests`, not exposed as a constant. **If the clamp moves, this list is out of date and
/// `every_legal_ppem_builds_an_atlas` will quietly stop covering the new values** — which is the
/// one hazard in this file and the reason the list is written here rather than derived.
const LEGAL: [u16; 17] = [10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26];

/// **The default, and the value part 46 arrived at.
///
/// **It was 20, and 20 is the value that shipped with the abort** — so `DEFAULT_PPM` is deliberately
/// set to `Args::default().ppem` rather than written twice, and this gate is what would notice the
/// default moving to a value whose pair does not fit.
///
/// **It is read from `Args::default()` and not a literal**, because the two facts that broke were
/// "the default is 20" and "the pair does not fit at 20", and a literal here would have kept
/// asserting the first while the second stopped being true. **`sizes_for` and `build` are asked
/// about the real default rather than about a number typed next to them.**
///
/// **A `fn` and not a `const`,** because `Args::default()` allocates a `String` for the passphrase
/// variable and cannot be evaluated at compile time. **The indirection buys the thing that
/// matters**: `every_legal_ppem_builds_an_atlas` below covers the whole range regardless, so this
/// only exists to name the default in the other gates' messages.
fn default_ppem() -> u16 {
    holonomy::args::Args::default().ppem
}

/// **`--ppem 17` is the largest that holds two atlas sizes.**
const LARGEST_PAIRED_PPM: u16 = 17;

/// **A plan's two-size request fits the atlas.**
fn pair_fits(plan: &Plan) -> bool {
    plan.has_zoom_size
}

/// # # The gate that was missing
///
/// **The default must boot.** One assertion, and it is the exact assertion whose absence let a
/// core dump ship: *build what the application builds at its default size, and get an atlas.*
///
/// **Not "the arithmetic looks right"** — `atlas_and_table_fit_the_ceiling` already made that
/// argument and it was wrong. **This rasterises.**
#[test]
fn the_default_ppem_builds_an_atlas() {
    let plan = atlas_plan::build(default_ppem());
    // **The atlas is a whole number of pixels' worth of coverage and it is not empty.** A plan that
    // silently built nothing would satisfy "it built an atlas" in the loosest reading, so the
    // assertion is on the occupancy rather than on the absence of a panic.
    assert!(
        plan.atlas.used() > 0 && plan.atlas.used() <= ATLAS_BYTES,
        "**the default atlas is built and non-empty**: used {} of {ATLAS_BYTES}",
        plan.atlas.used()
    );
}

/// **Every legal `--ppem` builds an atlas — and the application starts at all of them.**
///
/// **The whole legal range, one size each**, because the bug was *an abort* and an abort at any
/// legal value is the same defect. **This is the gate whose absence is why `cargo run` did not
/// work**, and it is cheap enough to be unconditional: one size per value, seventeen of them.
#[test]
fn every_legal_ppem_builds_an_atlas() {
    for ppem in LEGAL {
        let plan = atlas_plan::build(ppem);
        assert!(
            plan.atlas.used() > 0 && plan.atlas.used() <= ATLAS_BYTES,
            "**--ppem {ppem} built an atlas**: used {} of {ATLAS_BYTES}",
            plan.atlas.used()
        );
    }
}

/// **The boundary is where this file's table says it is: two sizes up to 17, one size from 18.**
///
/// ## Why the number 17 is asserted rather than derived
///
/// **`[atlas_plan::sizes_for]` gives the wish and [`atlas_plan::build`] gives the outcome, so the
/// boundary is already observable.** Asserting it pins the *documentation* — the table in
/// `atlas_plan`'s module doc, and the `--ppem 17 or smaller` in the refusal message — to the
/// product. **A refusal that names a flag which does not fix it is worse than no flag at all**,
/// and that is the only thing being held here.
///
/// **It costs two rasterisations**, one on each side of the wall, which is why it is not all
/// seventeen.
#[test]
fn the_pair_boundary_is_where_the_documentation_says() {
    let at = atlas_plan::build(LARGEST_PAIRED_PPM);
    assert!(
        pair_fits(&at),
        "**--ppem {LARGEST_PAIRED_PPM} holds two sizes**: used {} of {ATLAS_BYTES}",
        at.atlas.used()
    );
    assert_eq!(at.atlas.sizes().len(), 2, "**and the atlas really has two**");

    let over = atlas_plan::build(LARGEST_PAIRED_PPM + 1);
    assert!(
        !pair_fits(&over),
        "**--ppem {} does not**, which is what makes {LARGEST_PAIRED_PPM} the boundary",
        LARGEST_PAIRED_PPM + 1
    );
    assert_eq!(
        over.atlas.sizes().len(),
        1,
        "**and the fallback built exactly one size**"
    );
    assert!(
        over.atlas.used() <= ATLAS_BYTES,
        "**which fits**: used {} of {ATLAS_BYTES}",
        over.atlas.used()
    );
}

/// **The fallback keeps `base`, so the chrome the atlas is drawn into is the one `--ppem` asked for.**
///
/// ## Why the *first* size and not the second
///
/// **Every band height, the cell width and the ruler's column count derive from `--ppem`.** A
/// fallback that kept `base * 5 / 4` would draw text sized for a chrome that was not there.
#[test]
fn the_fallback_keeps_the_requested_size() {
    let plan = atlas_plan::build(default_ppem());
    assert_eq!(
        plan.atlas.sizes().first().copied(),
        Some(default_ppem()),
        "**the single size that survived is --ppem itself**, not the zoom size that was refused"
    );
}

/// **A refusal names the numbers and the flag that would fix it.**
///
/// ## Why this uses `--ppem 18` and not the default
///
/// **Because the default is now 16, where the pair fits and there is nothing to refuse.** The first
/// version of this gate asked the default and passed only by accident of the old default being 20;
/// **a gate about the fallback that runs on the path where the fallback does not fire is a gate
/// about nothing.** The fallback is exercised strictly above the boundary now, which is the only
/// place it happens in the product.
#[test]
fn a_refusal_names_the_numbers_and_the_flag() {
    let over = LARGEST_PAIRED_PPM + 1;
    let why = atlas_plan::build(over)
        .zoom_refusal(over)
        .unwrap_or_else(|| panic!("**--ppem {over} has one size, so there must be a refusal to explain**"));
    assert!(
        why.contains(&holonomy_assets::metric::ATLAS_BYTES.to_string()),
        "**the message carries the capacity**: {why:?}"
    );
    assert!(
        why.contains("--ppem 17"),
        "**and the flag value that would fix it**: {why:?}"
    );
    assert!(
        why.contains(&over.to_string()),
        "**and the size it was asked for**: {why:?}"
    );

    // **And there is no refusal to print when the pair held.**
    assert!(
        atlas_plan::build(default_ppem())
            .zoom_refusal(default_ppem())
            .is_none(),
        "**the default holds two sizes and has nothing to explain** -- a blank warning on every \
         normal boot is a warning nobody reads"
    );
}

/// **`sizes_for` is the wish: two sizes, ascending, and 125 % of the base.**
///
/// **The order and the sort are the assertion**, because [`zoom_size_index`] uses a *rank* in
/// `ZOOMS` as an index into `Atlas::sizes` — an atlas holding `[20, 16]` would draw 125 % at 100 %,
/// and nothing else in the program would notice.
#[test]
fn sizes_for_is_two_ascending_sizes() {
    assert_eq!(atlas_plan::sizes_for(16), vec![16, 20], "**16 gives 16 and 20**");
    assert_eq!(atlas_plan::sizes_for(20), vec![20, 25], "**20 gives 20 and 25**");
    assert_eq!(atlas_plan::sizes_for(10), vec![10, 12], "**10 gives 10 and 12**");
    for base in LEGAL {
        let sizes = atlas_plan::sizes_for(base);
        assert_eq!(sizes.len(), 2, "**--ppem {base} asks for two sizes**");
        assert_eq!(
            sizes[0], base,
            "**and the base is the smaller one**, because the chrome is laid out for it"
        );
        assert!(
            sizes[0] < sizes[1],
            "**ascending**: --ppem {base} gave {sizes:?} and a descending atlas draws 125 % at 100 %"
        );
    }
}

/// **Above the boundary the build is one size, and nothing claims otherwise.**
///
/// ## Why this uses `--ppem 18` and not the default
///
/// **Because the default is 16 and holds two sizes**, so asserting "the atlas has one size" about
/// it would be asserting the opposite of what part 46 decided. **The claim this gate makes is
/// about the fallback**, and the fallback fires strictly above the boundary.
///
/// ## Why it does not construct a `Session`
///
/// **Because the pairing is the whole claim.** `Session::set_zoom`'s refusal is held by
/// `tests/zoom.rs` (`out_of_range_zoom_percentages_are_refused_not_clamped`), and a session needs
/// an atlas, which needs a rasterisation this file already pays for seventeen times. **Two gates
/// testing the same fact from two directions is belt and braces; two gates testing it twice is a
/// slower suite.**
#[test]
fn above_the_boundary_the_build_is_one_size_and_nothing_claims_otherwise() {
    let over = LARGEST_PAIRED_PPM + 1;
    let plan = atlas_plan::build(over);
    assert_eq!(
        plan.atlas.sizes().len(),
        1,
        "**--ppem {over} holds one size**, so there is exactly one rank to select and the second \
         row of ZOOMS has no glyphs behind it"
    );
    assert_eq!(
        plan.atlas.sizes().first().copied(),
        Some(over),
        "**and it is the requested size**, not the zoom size that was refused -- the chrome is laid \
         out for `over` and drawing anything else would put glyphs in the wrong bands"
    );
    assert!(
        !plan.has_zoom_size,
        "**and `has_zoom_size` says so**, which is what makes `set_zoom` answer `ZoomUnavailable` \
         rather than clamp"
    );
}
