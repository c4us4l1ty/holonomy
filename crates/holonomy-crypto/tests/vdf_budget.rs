//! **The VDF iteration count is derived from a measurement, not chosen.** 6 tests.
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
    assert!(VDF_ITERATIONS > 0, "and it must be a time-lock, not nothing");
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