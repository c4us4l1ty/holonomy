//! `vdf-calibrate` — measure this host's cost per Montgomery squaring and print the constants.
//!
//! ```text
//! cargo run --release -p holonomy-crypto --bin vdf-calibrate
//! ```
//!
//! # What Plan/Plan.md §2.4 asks for
//!
//! ```text
//! NS_PER_SQUARING is measured by `vdf-calibrate` and re-derived on the target; T is a
//! compile-time constant computed from it.
//! ```
//!
//! **The measurement existed and the tool did not.** `modulus::tests::calibrate_ns_per_squaring`
//! does exactly this work — and it is `#[ignore]`d, inside a test module, and prints to `stderr`
//! rather than producing a value a person can paste. So `MEASURED_NS_PER_SQUARING` in
//! `envelope.rs` is a hand-copied number, and §2.4's "re-derived on the target" has no mechanism.
//!
//! # Why `--release` is not optional, and what happens without it
//!
//! **This tool refuses to print a paste-ready constant from an unoptimised build, and exits
//! non-zero.** It is the same failure as four red gates this tree carried for a phase: a number
//! measured in a debug build is a property of the compiler, not of the implementation. At
//! `opt-level = 0` the squaring chain measures roughly **ten times slower**, and a `T` derived from
//! it would be ten times too small — **a time-lock that costs a tenth of its budget, silently, and
//! looks like it was calibrated.**
//!
//! `--allow-unoptimised` prints the number anyway, marked, for the case where you are deliberately
//! calibrating a debug build (the crate's own tests do this).
//!
//! # Why a binary and not a `build.rs`
//!
//! §2.4 says "baked in from a calibration run", which sounds like `build.rs`. **A build script is
//! compiled unoptimised**, so a `build.rs` that measured this would measure its own debug build and
//! bake in a wrong constant — automatically, on every build, with no human in the loop to notice.
//!
//! **So the sequence is deliberate**: measure with an optimised binary, commit the number, and let
//! the *committed* number feed `T` at compile time. A human pastes one value between two runs, and
//! `vdf_budget::the_baked_constant_equals_its_own_derivation` fails the moment they paste one
//! constant and not the other.

use std::process::ExitCode;
use std::time::Instant;

use holonomy_crypto::bignum::ZERO;
use holonomy_crypto::envelope::{vdf_iterations_for, TARGET_VDF_MS};
use holonomy_crypto::modulus::sequential_squarings;

/// Squarings per sample. Long enough that the timer is not measuring its own resolution.
const ITERS: u64 = 20_000;
/// Samples, and the statistic is the **median**.
///
/// **The median and not the mean**, because the distribution on this host has a tail: the test's own
/// doc comment records a spread of 2,580–2,806 ns, and a mean sits nearer the top of that band than
/// the centre. A calibration whose answer is decided by one scheduler hiccup is not a calibration.
const SAMPLES: usize = 5;

fn main() -> ExitCode {
    let allow_unoptimised = std::env::args().any(|a| a == "--allow-unoptimised");

    println!("holonomy vdf-calibrate");
    println!("  modulus           : 2048-bit committed safe prime (Npub)");
    println!("  profile           : {}", profile());
    println!("  budget            : {TARGET_VDF_MS} ms");
    println!("  per sample        : {ITERS} squarings, {SAMPLES} samples");

    if cfg!(debug_assertions) && !allow_unoptimised {
        eprintln!();
        eprintln!("REFUSING to print a constant from an unoptimised build.");
        eprintln!();
        eprintln!(
            "  At `opt-level = 0` this chain measures roughly 10x slower than it will ship, and a\n\
             `T` derived from it would be 10x too small. That is a time-lock costing a tenth of\n\
             its budget, and nothing downstream would say so -- the constant is just a number."
        );
        eprintln!();
        eprintln!("  cargo run --release -p holonomy-crypto --bin vdf-calibrate");
        eprintln!();
        eprintln!(
            "  (--allow-unoptimised prints the number marked as unusable, for calibrating a debug\n\
             build on purpose.)"
        );
        return ExitCode::FAILURE;
    }

    // **Warm up before the first timed sample.** The first chain faults in the pages and misses
    // icache on every branch; measuring that is measuring the allocator, not the squaring.
    let mut seed = ZERO;
    seed[0] = 0xDEAD_BEEF;
    if let Err(e) = sequential_squarings(&seed, 2_000) {
        eprintln!("warm-up failed: {e}");
        return ExitCode::FAILURE;
    }

    let mut samples = Vec::with_capacity(SAMPLES);
    for i in 0..SAMPLES {
        let t0 = Instant::now();
        if let Err(e) = sequential_squarings(&seed, ITERS) {
            eprintln!("sample {i} failed: {e}");
            return ExitCode::FAILURE;
        }
        let ns = t0.elapsed().as_nanos() as f64 / ITERS as f64;
        println!("  sample {i}: {ns:>9.1} ns/squaring");
        samples.push(ns);
    }

    samples.sort_by(|a, b| a.partial_cmp(b).expect("no NaN from a timer"));
    let median = samples[samples.len() / 2];
    let lo = samples[0];
    let hi = samples[samples.len() - 1];
    let spread = (hi - lo) / lo * 100.0;

    println!();
    println!("  median            : {median:.1} ns");
    println!("  spread            : {lo:.1} .. {hi:.1} ns  ({spread:.1}%)");
    if spread > 20.0 {
        println!(
            "  WARNING           : a spread above 20% means the host was busy. Run again on an idle\n\
             \x20                     machine before pasting anything."
        );
    }

    // **Round DOWN, for the reason `vdf_iterations_for` rounds down**: a budget is a ceiling, so it
    // is enforced as one. Rounding the *cost* down gives the *most* iterations, which is the wrong
    // direction — so the cost is rounded down and `vdf_iterations_for` then floors `T`, and the two
    // floorings compose into "never over budget".
    let rounded = median.floor() as u64;
    let iterations = match vdf_iterations_for(TARGET_VDF_MS, rounded) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("could not derive T: {e}");
            return ExitCode::FAILURE;
        }
    };

    println!();
    println!("Paste into crates/holonomy-crypto/src/envelope.rs:");
    println!();
    println!("    pub const MEASURED_NS_PER_SQUARING: u64 = {rounded};");
    println!("    pub const VDF_ITERATIONS: u64 = {iterations};");
    println!();
    println!(
        "  `vdf_budget::the_baked_constant_equals_its_own_derivation` fails unless VDF_ITERATIONS is\n\
         exactly `vdf_iterations_for({TARGET_VDF_MS}, {rounded})`, which is {iterations}."
    );
    if cfg!(debug_assertions) {
        println!();
        println!(
            "  (--allow-unoptimised was passed: these constants are UNUSABLE. The chain measured\n\
             {median:.1} ns in an unoptimised build.)"
        );
    }
    ExitCode::SUCCESS
}

/// `opt-level` is not readable at runtime, so the build profile is inferred from whether
/// `debug_assertions` are on — which is the same signal, and the one every gate in this tree uses.
fn profile() -> &'static str {
    if cfg!(debug_assertions) {
        "debug (debug_assertions on -- opt-level 0)"
    } else {
        "release (debug_assertions off -- optimised)"
    }
}