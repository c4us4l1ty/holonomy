//! **The VDF iteration count is derived from a measurement, not chosen.** 7 tests.
//!
//! # Why this gate exists
//!
//! PROJECT.md §1.1 and §2.4 record that **every hard-coded VDF parameter in this tree's history has been
//! wrong by 3–7×**: the PRD claimed 300 ns per squaring where 2,077 ns was measured, and 180 ms for
//! Argon2id where 524 ms was measured. The PRD then also specified a hard-coded `T = 1,500,000`, which §1.1
//! measured at **3,115 ms** — 7× its intended 450 ms.
//!
//! So the failure mode is not "the number is a bit off". **It is that a number nobody derived was trusted
//! for a security property.** This gate makes the derivation the only route to a `T`, and pins the
//! arithmetic that replaces the guesses.
//!
//! | what it proves | test |
//! | --- | --- |
//! | the formula is what §2.4 specifies | [`the_iteration_count_is_the_floor_of_the_documented_formula`] |
//! | and the baked constant *is* that derivation | [`the_baked_constant_equals_its_own_derivation`] |
//! | the budget bounds the cost | [`the_derived_count_costs_about_the_budget_and_not_more`] |
//! | a faster host gets more squarings | [`a_faster_host_gets_strictly_more_squarings`] |
//! | degenerate inputs are refused, not clamped | [`degenerate_inputs_are_refused_rather_than_clamped`] |
//! | and the SDK's own arithmetic is consistent | [`the_modulus_agrees_about_what_zero_iterations_means`] |
//! | **and the constant still describes this host** | [`the_committed_cost_still_describes_this_host`] |

use holonomy_crypto::envelope::{
    vdf_iterations_for, EnvelopeError, TARGET_VDF_MS, VDF_ITERATIONS, MEASURED_NS_PER_SQUARING,
};
use holonomy_crypto::modulus::VdfError;

/// **§2.4's formula, spelled out.** `T = floor(target_ms * 1e6 / ns_per_squaring)`.
///
/// Written out longhand rather than compared against the function's own output, because a test that
/// recomputes the implementation proves only that the implementation is idempotent.
#[test]
fn the_iteration_count_is_the_floor_of_the_documented_formula() {
    for (ms, ns) in [(250u64, 2_664u64), (250, 2_077), (400, 2_664), (100, 10_000), (1, 1)] {
        let longhand = (ms * 1_000_000) / ns;
        assert_eq!(
            vdf_iterations_for(ms, ns).expect("derivable"),
            longhand,
            "{ms} ms at {ns} ns/squaring is {longhand}"
        );
    }
}

/// **The baked constant is derived, not typed.** If someone edits `VDF_ITERATIONS` by hand this fails,
/// which is the entire point: the number must be *the output of the derivation* on this host.
#[test]
fn the_baked_constant_equals_its_own_derivation() {
    assert_eq!(
        VDF_ITERATIONS,
        vdf_iterations_for(TARGET_VDF_MS, MEASURED_NS_PER_SQUARING).expect("derive"),
        "VDF_ITERATIONS must be TARGET_VDF_MS worth of squarings at MEASURED_NS_PER_SQUARING -- \
         edit one and this fails, which is the point of deriving it"
    );
    // **In a `const` block, so this is checked when the crate is compiled rather than when the test
    // runs.** `VDF_ITERATIONS` is a constant; asserting about it at run time means a build that ships
    // a zero iteration count compiles fine and fails here, **once someone remembers to run the
    // tests**. A constant assertion is a compile error, which is the earlier and stronger instrument.
    //
    // **The runtime assertion above is kept and is not the same claim**: that one says the derived
    // value equals the target, and it needs both values, so it cannot be a `const` check.
    const { assert!(VDF_ITERATIONS > 0, "a time-lock, not nothing") };
}

/// **The budget is a ceiling.** `floor` bounds the cost above and never below the target.
///
/// The rounding direction is the whole content of this test: rounding *up* would let a host slower than
/// the calibration host overshoot the budget, and the budget exists to bound the user's wait.
#[test]
fn the_derived_count_costs_about_the_budget_and_not_more() {
    let t = VDF_ITERATIONS;
    let cost_ms = (t as f64 * MEASURED_NS_PER_SQUARING as f64) / 1e6;
    assert!(
        cost_ms <= TARGET_VDF_MS as f64,
        "{t} squarings at {MEASURED_NS_PER_SQUARING} ns is {cost_ms:.1} ms, over the \\
         {TARGET_VDF_MS} ms budget -- the derivation must floor, never round up"
    );
    // And not wildly under, which would mean the budget is not being met at all.
    assert!(
        cost_ms > TARGET_VDF_MS as f64 * 0.99,
        "{cost_ms:.1} ms is far under the {TARGET_VDF_MS} ms budget, so the budget is not being met"
    );
}

/// **The dependence on host speed is real and monotone.** This is the property the PRD's hard-coded
/// `T = 1,500,000` violated: at 300 ns/squaring it costs 450 ms, at 2,077 ns it costs 3,115 ms.
#[test]
fn a_faster_host_gets_strictly_more_squarings() {
    let slow = vdf_iterations_for(TARGET_VDF_MS, 3_115).expect("slow host");
    let measured = vdf_iterations_for(TARGET_VDF_MS, MEASURED_NS_PER_SQUARING).expect("this host");
    let fast = vdf_iterations_for(TARGET_VDF_MS, 300).expect("the PRD's claim");

    assert!(fast > measured, "a faster host gets more squarings: {fast} > {measured}");
    assert!(measured > slow, "than a slower one: {measured} > {slow}");

    // And the consequence the PRD got wrong, stated as arithmetic: 1,500,000 squarings costs 450 ms at
    // 300 ns and **7x that** at the 2,077 ns §1.1 actually measured. This is why `T` is derived.
    let prd_t = 1_500_000u64;
    let at_300_ms = prd_t as f64 * 300.0 / 1e6;
    let at_2077_ms = prd_t as f64 * 2_077.0 / 1e6;
    assert!(
        at_2077_ms > at_300_ms * 6.0,
        "the PRD's T costs {at_300_ms:.0} ms at its claimed speed and {at_2077_ms:.0} ms at the measured \
         one; if that ratio is not ~7x then §1.1's numbers are wrong and this test is protecting a \
         mistake"
    );
}

/// **Degenerate inputs are refused, not clamped.** `ns == 0` would divide by zero and report an instant
/// VDF — the worst possible outcome for a security parameter — and `ms == 0` would produce `T = 0`,
/// which is not a time-lock.
#[test]
fn degenerate_inputs_are_refused_rather_than_clamped() {
    assert_eq!(
        vdf_iterations_for(TARGET_VDF_MS, 0).expect_err("zero cost must not divide"),
        EnvelopeError::Vdf(VdfError::Uncalibrated),
        "an uncalibrated host must be refused, and named as uncalibrated rather than as a bad seed"
    );
    assert_eq!(
        vdf_iterations_for(0, MEASURED_NS_PER_SQUARING).expect_err("zero budget is not a budget"),
        EnvelopeError::Vdf(VdfError::ZeroIterations),
        "a zero budget must be refused rather than clamped to some minimum chain"
    );
    // The refusal is distinct from the seed failure it used to borrow, so a reader is not sent to look
    // at the seed for a measurement that never happened.
    assert_ne!(
        VdfError::Uncalibrated,
        VdfError::ZeroSeed,
        "the uncalibrated case must be its own variant"
    );
}

/// **Overflow refuses rather than wrapping.** `target_ms * 1e6` overflows `u64` at about 1.8e13 ms, which
/// is a number a caller can type; a wrapped value would derive a *small* `T` from a *huge* budget, which is
/// the dangerous direction.
#[test]
fn the_modulus_agrees_about_what_zero_iterations_means() {
    let huge = vdf_iterations_for(u64::MAX, MEASURED_NS_PER_SQUARING).expect("saturating");
    assert!(
        huge > VDF_ITERATIONS,
        "an absurd budget must saturate to a very large count ({huge}), not wrap to a small one"
    );
    assert!(huge < u64::MAX, "and it must still be a count, not saturating to the type's maximum");

    // And the chain refuses zero, so `T = 0` is caught at both ends rather than only at the derivation.
    let seed = holonomy_crypto::modulus::seed_from_k_int(&[7u8; 64]);
    assert!(
        holonomy_crypto::modulus::sequential_squarings(&seed, 0).is_err(),
        "the chain itself refuses zero iterations, so a zero T cannot reach it"
    );
}
/// **The constant still describes this host, measured rather than assumed.** 7 tests.
///
/// # What the five above could not see
///
/// Every one of them compares `VDF_ITERATIONS` against `MEASURED_NS_PER_SQUARING`, and
/// `the_derived_count_costs_about_the_budget_and_not_more` multiplies one constant by the other.
/// **Both are self-consistent by construction**, so a committed cost that stopped describing any real
/// host — a machine three times faster, a CI runner, an ARM target — would pass all five and make
/// `T` wrong by the same factor.
///
/// # Why the band is asymmetric
///
/// The security direction is **not spending less than the budget**. `MEASURED_NS_PER_SQUARING` is
/// deliberately rounded *down*, so on a host slower than the one it was taken on, `T` buys *more*
/// time than intended — which is safe, just slower to unlock.
///
/// Measured on this host on 2026-10-12 with the `vdf-calibrate` tool:
///
/// ```text
/// run 1 (load 2.4): median 2,981 ns, spread 24.4%  -- the tool's own spread warning fired
/// run 2 (load 1.1): median 3,168 ns, spread  8.0%
/// committed:         2,664 ns  ->  T = 93,843
/// actual at 3,168:   297 ms against a 250 ms budget, 1.19x over
/// ```
///
/// **Which is why the committed constants were left alone.** `T` is sized for a host faster than this
/// one, so the time-lock runs *longer* than its budget here — the safe direction. Pasting 3,168 would
/// have cut `T` to 78,914, **weakening the time-lock by 16 % on the strength of a measurement taken
/// on a machine that is not the deployment target.** §2.4 says "re-derived on the target"; this host
/// is not the target.
///
/// **The floor at 0.5× is the one that earns the gate.** Underspending by more than 2× means `T` is
/// too small and the time-lock is weaker than its budget claims; that is the failure worth catching,
/// and no constant-only test can catch it.
#[test]
fn the_committed_cost_still_describes_this_host() {
    // **The measurement is release-profile.** A debug build measures this chain roughly ten times
    // slower, which would put the real cost at ~3,000 ms -- twelve times the budget -- and the gate
    // would fail for the same reason four others in this tree carried for a phase. Measured on an
    // unmodified tree before the diagnosis, so it predates any of this.
    //
    // cargo test -p holonomy-crypto --release --test vdf_budget
    if cfg!(debug_assertions) {
        eprintln!(
            "skipping: the cost per squaring is a release-profile number (opt-level=0 measures it \
             ~10x slower, which would report a 12x overspend). Re-run with \
             `cargo test -p holonomy-crypto --release --test vdf_budget`. `vdf-calibrate` is the \
             tool that produces this figure for a real build."
        );
        return;
    }

    use holonomy_crypto::bignum::ZERO;
    use holonomy_crypto::modulus::sequential_squarings;
    use std::time::Instant;

    let mut seed = ZERO;
    seed[0] = 0xDEAD_BEEF;
    // Warm up, for the reason the tool does: the first chain faults pages in and misses icache.
    sequential_squarings(&seed, 2_000).expect("warm-up");

    // **A short chain relative to `VDF_ITERATIONS`**, because this gate runs in a suite and 93,843
    // squarings at ~3 us is 300 ms per sample. 20,000 is 64 ms and is the tool's own choice.
    const ITERS: u64 = 20_000;
    let t0 = Instant::now();
    sequential_squarings(&seed, ITERS).expect("chain");
    let ns = (t0.elapsed().as_nanos() as f64 / ITERS as f64).round() as u64;

    let actual_ms = VDF_ITERATIONS as f64 * ns as f64 / 1e6;
    let ratio = actual_ms / TARGET_VDF_MS as f64;
    println!(
        "VDF_ITERATIONS {VDF_ITERATIONS} at {ns} ns/squaring costs {actual_ms:.0} ms \
         against a {TARGET_VDF_MS} ms budget ({ratio:.2}x); the constant says \
         {MEASURED_NS_PER_SQUARING} ns"
    );

    assert!(
        ratio >= 0.5,
        "**{VDF_ITERATIONS} squarings cost {actual_ms:.0} ms here, under half the {TARGET_VDF_MS} ms \
         budget.** The committed {MEASURED_NS_PER_SQUARING} ns is far below the {ns} ns measured, so \
         `T` is sized for a host several times faster than this one and the time-lock is weaker than \
         its budget claims. Re-derive with `vdf-calibrate` on the deployment target."
    );
    assert!(
        ratio <= 2.0,
        "**{VDF_ITERATIONS} squarings cost {actual_ms:.0} ms here, more than twice the \
         {TARGET_VDF_MS} ms budget.** The committed {MEASURED_NS_PER_SQUARING} ns is well above the \
         {ns} ns measured. Overspending is the safe direction -- a slower unlock is not a weaker \
         time-lock -- but 2x means unlock latency is now a usability problem and the constant is \
         stale."
    );
}
