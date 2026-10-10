//! **Phase 13 part 6's gate: styling is a function of the document's bytes, not of what is resident.**
//! 7 tests.
//!
//! # The invariant
//!
//! **A byte's style must not depend on whether its leaf happens to be held.** A document loaded from a
//! container cannot be styled up front — the bytes are encrypted — so `SpanMap::plain(text_len)` makes a
//! claim about content that nobody has checked. This gate pins the mechanism that makes the claim honest:
//! [`SpanMap::observe`] records what was read, [`SpanMap::style_at_known`] refuses to guess beyond it, and
//! **the answer for a byte is the same whether it arrived on the first fault or the fifth.**
//!
//! | what it proves | test |
//! | --- | --- |
//! | unread is distinguishable from plain | [`unread_bytes_are_not_reported_as_known_plain`] |
//! | the watermark is monotone | [`the_read_watermark_never_goes_backwards`] |
//! | **the answer survives eviction** | [`style_does_not_depend_on_residency`] |
//! | an unstyled leaf is still a finding | [`an_unstyled_region_still_advances_the_watermark`] |
//! | re-observing is idempotent | [`re_observing_a_prefix_is_idempotent_not_an_error`] |
//! | gaps and overruns are refused | [`a_gappy_or_overrunning_observation_is_refused`] |
//! | and a refused one changes nothing | [`a_refused_observation_leaves_the_map_untouched`] |

use holonomy_text::{SpanError, SpanMap, TextIntervalSpan, STYLE_BOLD};

/// A bold run over `[start, end)`.
fn bold(start: u32, end: u32) -> TextIntervalSpan {
    TextIntervalSpan {
        start_byte: start,
        end_byte: end,
        style_flags: STYLE_BOLD,
        color_rgb: 0,
    }
}

fn plain(start: u32, end: u32) -> TextIntervalSpan {
    TextIntervalSpan::plain(start, end)
}

/// **`plain` is a claim nobody has checked, and the map can now say so.** This is the whole reason the
/// watermark exists: without it, a document full of `**bold**` reports plain for every byte and the
/// difference between "plain" and "unknown" is invisible.
#[test]
fn unread_bytes_are_not_reported_as_known_plain() {
    let m = SpanMap::plain(1_000);
    assert_eq!(
        m.read_through(),
        0,
        "a plain map has read nothing, however plain it claims to be"
    );
    assert!(!m.is_read(0), "so offset 0 is not known");
    assert_eq!(
        m.style_at_known(0),
        None,
        "and style_at_known says so rather than guessing"
    );
    // `style_at` still answers. **CORRECTION, part 24: the reason given here was wrong.** This said
    // "`style_at` still answers, because it is on the paint path and must not change shape yet" — and it
    // was not on the paint path. `emit_body_text` emitted one `DocRun` per line with `Style::REGULAR`
    // hardcoded, and `style_at` was called from no product code at all: only from this crate's tests. So
    // the stopgap was justified by a coupling that did not exist.
    //
    // **It is on the paint path now**, because `emit_body_text` walks `runs_in` and resolves each run's
    // `atlas_style`. So the sentence is true as of part 24 and was false until it.
    assert_eq!(
        m.style_at(0).style_flags,
        0,
        "style_at still returns plain for an unread byte -- that is the documented stopgap, and it is \\
         exactly why style_at_known exists alongside it"
    );
    assert!(
        m.spans().len() == 1 && m.spans()[0].end_byte == 1_000,
        "one plain run over the whole document"
    );
}

/// **The watermark only increases.** A byte that has been read does not become unread, so this is not a
/// residency map — and conflating the two is the mistake the field's doc comment warns about.
#[test]
fn the_read_watermark_never_goes_backwards() {
    let mut m = SpanMap::plain(1_000);
    m.observe(400, &[plain(0, 400)]).expect("observe a prefix");
    assert_eq!(m.read_through(), 400);
    m.observe(700, &[plain(0, 700)]).expect("observe more");
    assert_eq!(m.read_through(), 700, "it advanced");
    // Going backwards is a no-op, not an error and not a rewind.
    m.observe(200, &[plain(0, 200)])
        .expect("a smaller prefix is allowed");
    assert_eq!(
        m.read_through(),
        700,
        "and it did NOT rewind -- a prefix re-read cannot un-read a suffix"
    );
    assert!(m.is_read(699), "so a byte read earlier is still read");
}

/// **The invariant this whole file exists for.** A styled byte reports bold whether it arrived on the
/// first fault or after the map has been filled in twice, and **evicting and re-reading changes nothing.**
#[test]
fn style_does_not_depend_on_residency() {
    // The document's first 200 bytes are bold, the rest plain.
    let learned = |through: u32| -> Vec<TextIntervalSpan> {
        let mut v = Vec::new();
        if through > 0 {
            v.push(bold(0, 200.min(through)));
        }
        if through > 200 {
            v.push(plain(200, through));
        }
        v
    };

    // Path 1: read everything in one go.
    let mut once = SpanMap::plain(1_000);
    once.observe(1_000, &learned(1_000)).expect("observe all");

    // Path 2: read in two passes, with the *first* pass re-observed afterwards -- the shape of a leaf
    // evicted and faulted back in.
    let mut twice = SpanMap::plain(1_000);
    twice.observe(200, &learned(200)).expect("first window");
    let first_reading = twice.style_at_known(10);
    twice
        .observe(1_000, &learned(1_000))
        .expect("then all of it");
    twice
        .observe(200, &learned(200))
        .expect("and the first window again, as an eviction would");

    assert_eq!(
        once.style_at(10),
        twice.style_at(10),
        "bold at offset 10 either way"
    );
    assert_eq!(
        once.style_at(500),
        twice.style_at(500),
        "plain at offset 500 either way"
    );
    assert_eq!(
        first_reading.map(|s| s.style_flags),
        Some(STYLE_BOLD),
        "and it was bold when first read"
    );
    assert_eq!(
        once.spans(),
        twice.spans(),
        "the span lists converge: arriving in two pieces leaves the same map as arriving in one"
    );
}

/// **An unstyled region is still a finding.** If the watermark only advanced when spans were learned, a
/// genuinely plain document would leave every region permanently "unknown" — and the map would never
/// learn that a document with no styling *is* plain. `learned.is_empty()` is the common case, not a no-op.
#[test]
fn an_unstyled_region_still_advances_the_watermark() {
    let mut m = SpanMap::plain(1_000);
    assert!(m.style_at_known(0).is_none(), "unknown before");

    // An all-plain document, learned as a single plain run -- and separately, as *nothing at all*.
    m.observe(0, &[])
        .expect("a zero-length observation is legal");
    let mut empty = SpanMap::plain(1_000);
    empty.observe(0, &[]).expect("same");
    assert_eq!(m.read_through(), empty.read_through());

    // The substantive case: an all-plain *region* must still count as read.
    let mut region = SpanMap::plain(1_000);
    region
        .observe(300, &[plain(0, 300)])
        .expect("an all-plain region");
    assert_eq!(region.read_through(), 300);
    assert_eq!(
        region.style_at_known(299).map(|s| s.style_flags),
        Some(0),
        "so offset 299 is *known* to be plain, which is different from unknown"
    );
    assert!(
        region.style_at_known(300).is_none(),
        "and 300 is still unknown"
    );
}

/// **Re-observing a prefix is idempotent, not an error.** A leaf can be evicted and faulted back in, so
/// the second visit must reach the same state as the first — that is the "indistinguishable from resident"
/// property, and it is why `observe` treats a smaller `through` as a no-op rather than an error.
#[test]
fn re_observing_a_prefix_is_idempotent_not_an_error() {
    let mut m = SpanMap::plain(1_000);
    m.observe(600, &[bold(0, 200), plain(200, 600)])
        .expect("first");
    let before = m.spans().to_vec();

    m.observe(600, &[bold(0, 200), plain(200, 600)])
        .expect("identical re-observation");
    assert_eq!(m.spans(), &before[..], "and it changed nothing");

    // Re-observing a *smaller* prefix with the same content is also a no-op, not a truncation.
    m.observe(200, &[bold(0, 200)]).expect("smaller prefix");
    assert_eq!(
        m.spans(),
        &before[..],
        "a smaller prefix must not truncate the map"
    );
    assert_eq!(m.read_through(), 600, "nor move the watermark back");
}

/// **A gappy or overrunning observation is refused.** A gap is a claim about a byte range the map was not
/// told about, and an overrun is a claim past what was read — either would assert certainty nobody has.
#[test]
fn a_gappy_or_overrunning_observation_is_refused() {
    let mut m = SpanMap::plain(1_000);

    // A gap: starts at 100 rather than 0.
    let gappy = m
        .observe(300, &[plain(100, 300)])
        .expect_err("a gap must be refused");
    assert!(
        matches!(gappy, SpanError::RangeOutOfBounds { .. }),
        "got {gappy:?}"
    );

    // An overrun: ends past `through`.
    let overrun = m
        .observe(300, &[plain(0, 400)])
        .expect_err("past `through` must be refused");
    assert!(
        matches!(overrun, SpanError::RangeOutOfBounds { .. }),
        "got {overrun:?}"
    );

    // Short of `through`: the tail is unaccounted for.
    let short = m
        .observe(300, &[plain(0, 100)])
        .expect_err("a short cover must be refused");
    assert!(
        matches!(short, SpanError::RangeOutOfBounds { .. }),
        "got {short:?}"
    );

    // Past the document.
    let past_doc = m
        .observe(2_000, &[plain(0, 2_000)])
        .expect_err("past the document");
    assert!(
        matches!(past_doc, SpanError::OutOfBounds { .. }),
        "got {past_doc:?}"
    );
}

/// **A refused observation leaves the map byte-for-byte as it was.** The watermark is advanced *after*
/// the splice, so an error cannot half-apply — which is the failure a caller cannot recover from.
#[test]
fn a_refused_observation_leaves_the_map_untouched() {
    let mut m = SpanMap::plain(1_000);
    m.observe(500, &[bold(0, 200), plain(200, 500)])
        .expect("a good observation first");
    let spans_before = m.spans().to_vec();
    let read_before = m.read_through();

    assert!(
        m.observe(800, &[plain(300, 800)]).is_err(),
        "a gappy observation is refused"
    );
    assert_eq!(
        m.read_through(),
        read_before,
        "and the watermark did not move"
    );
    assert_eq!(m.spans(), &spans_before[..], "and the spans are unchanged");

    // Still usable afterwards, which is the point of not half-applying.
    m.observe(1_000, &[bold(0, 200), plain(200, 1_000)])
        .expect("a later good observation works");
    assert_eq!(m.read_through(), 1_000);
}

/// **Styling an empty map works, and used to silently do nothing.**
///
/// `style_range` rebuilds the span list from the spans already present, so on a map with no spans it
/// returned `Ok(())` and styled nothing — and `empty_over`'s own documentation invited exactly that,
/// promising the default style "until the first span is added" while offering no way to add one.
/// Silent, and wrong in the direction that looks fine: the document comes back unstyled rather than
/// refusing to open.
///
/// Found by writing the span-table gate, not by reading this module — which is the argument for gates
/// that build documents rather than assert on hand-built maps.
#[test]
fn styling_an_empty_map_is_not_a_silent_no_op() {
    let mut m = SpanMap::empty_over(1_000);
    assert!(m.spans().is_empty(), "it starts with no spans at all");
    assert_eq!(
        m.style_at(500).style_flags,
        0,
        "and reports the default style"
    );

    m.style_range(100, 200, STYLE_BOLD, 0)
        .expect("styling a range on an empty map");

    assert_eq!(
        m.style_at(150).style_flags,
        STYLE_BOLD,
        "the range IS styled -- previously this returned Ok and left the map untouched"
    );
    assert_eq!(m.style_at(50).style_flags, 0, "outside it, still plain");
    assert_eq!(m.style_at(900).style_flags, 0, "and after it, still plain");
    assert_eq!(
        m.spans().iter().map(|s| s.len()).sum::<u32>(),
        1_000,
        "the map is still gap-free over the whole document, which is what the seeding buys"
    );
    // And the map's own invariants, asserted here rather than left to the crate-private checker:
    // sorted, non-overlapping, gap-free, ending exactly at `text_len`.
    let spans = m.spans();
    assert_eq!(
        spans.first().map(|s| s.start_byte),
        Some(0),
        "it starts at 0"
    );
    assert_eq!(
        spans.last().map(|s| s.end_byte),
        Some(1_000),
        "and ends at the document length"
    );
    assert!(
        spans.windows(2).all(|w| w[0].end_byte == w[1].start_byte),
        "and has no gap and no overlap between adjacent spans: {:?}",
        spans
            .iter()
            .map(|s| (s.start_byte, s.end_byte))
            .collect::<Vec<_>>()
    );
}
