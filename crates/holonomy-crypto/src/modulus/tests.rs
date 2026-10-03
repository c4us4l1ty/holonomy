//! Phase 1 gate for the modulus: `cargo test -p holonomy-crypto modulus`.
//!
//! Everything here is O(1) with respect to the size of a proof search -- no candidate is
//! ever generated. The chain tests use short iteration counts on purpose: they check that
//! the arithmetic is right, not that it is slow, and the per-squaring cost is calibrated
//! separately in Phase 2.

use super::*;
use crate::bignum::{bit_len, cmp, from_hex, is_odd, mul_mod_small, to_hex, ONE, ZERO};

/// The constant is the committed literal, is 2048 bits, and is odd.
///
/// Oddness is not cosmetic: Montgomery multiplication requires `gcd(N, R) = 1`, and
/// `R = 2^2048`, so an even modulus would make the whole scheme undefined rather than
/// merely slow.
#[test]
fn n_pub_is_the_committed_2048_bit_constant() {
    assert_eq!(bit_len(&N_PUB), 2048, "N_pub must be exactly 2048 bits");
    assert!(
        is_odd(&N_PUB),
        "N_pub must be odd for Montgomery arithmetic"
    );
    // `to_hex` emits a "0x" prefix; the committed literal does not.
    let rendered = to_hex(&N_PUB);
    assert_eq!(
        &rendered[2..],
        n_pub_hex(),
        "parsed constant must match the committed literal"
    );

    // The compile-time parser and the runtime one must agree. If they ever diverge, the
    // constant in .rodata and the literal in the source are describing different numbers.
    assert_eq!(Some(N_PUB), from_hex(n_pub_hex()));
}

/// A random 2048-bit modulus would almost certainly also be odd and 2048 bits, so those
/// two assertions alone prove very little. Pin the actual digits: this is the constant
/// that the whole time-lock's soundness rests on, so a one-nibble change must fail the
/// build rather than silently produce a different modulus.
#[test]
fn n_pub_digits_are_pinned() {
    const PINNED_HEAD: &str = "c7970ceedcc3b0754490201a7aa613cd73911081c790f5f1a8726f463550bb5b";
    const PINNED_TAIL: &str = "3131f55615172866bccc30f95054c824e733a5eb6817f7bc16399d48c6361cc7e5";
    let h = to_hex(&N_PUB);
    assert_eq!(
        &h[2..2 + PINNED_HEAD.len()],
        PINNED_HEAD,
        "high 32 bytes changed"
    );
    assert_eq!(
        &h[h.len() - PINNED_TAIL.len()..],
        PINNED_TAIL,
        "low 32 bytes changed"
    );
    assert_eq!(h.len(), 2 + 512, "must be 512 nibbles");
}

/// Montgomery arithmetic must work against *this* modulus, not just against the test
/// prime used in the bignum vectors.
#[test]
fn montgomery_roundtrips_against_n_pub() {
    let p = mont_params();
    let mut seed = [0u8; 64];
    for (i, b) in seed.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(7).wrapping_add(1);
    }
    let x = seed_from_k_int(&seed);
    assert!(cmp(&x, &N_PUB) == Ordering::Less);

    let m = to_mont(&x, &N_PUB, p.n0inv, &p.r2);
    assert!(
        cmp(&m, &N_PUB) == Ordering::Less,
        "Montgomery form must stay reduced"
    );
    assert_eq!(mont_mul(&m, &ONE, &N_PUB, p.n0inv), x);
}

/// A short chain against CPython. `S_0 = 0xDEADBEEF`, squared 64 times mod N_pub.
///
/// A 64-step chain is ~130 microseconds, so this stays a unit test. It still crosses the
/// whole Montgomery path 64 times, which is what catches a chain that is off by one step
/// or forgets the final conversion out of Montgomery form -- both of which produce
/// plausible-looking wrong answers.
#[test]
fn chain_of_64_matches_python() {
    let mut s0 = ZERO;
    s0[0] = 0xDEAD_BEEF;
    const EXPECTED: &str = "0x69e87b47fa10f763451cc69ab820070d4a47c67cecebb861bb9d2c7e68459eeb7d323b11646ad9ac55b9babe2bf8912d1df5aea221d0d0a6126b2833987c8a44ad33efff5a680b6d500d2ed18355f7916b0c6648dc0c12a2b69b78b22af6dea1b5316e61866cd15337ad00f219f1cd4aff62150b09318209aa2b15eebc6caae78be6d6b5136f3fe2fa6bfac6011d631f1be5d19fc77535b9eb04cf574ff8a42a55d7514a238371bccb808769a8d3d9ccfb85ad064b37ff381bf29edcc52e5b4ab3ed106b8dc9ea66c016328a96f366650fbc01aade37d3af1457d74639c2e0fec2625188108ec55eb3328d6fed0518f27da0a5a24cd6f65a6fd6b733847736fc";
    let got = sequential_squarings(&s0, 64).expect("chain");
    assert_eq!(to_hex(&got), EXPECTED);
}

/// Same chain length, full-width seed. Guards against the narrow seed above passing only
/// because it never exercises the high limbs.
#[test]
fn chain_of_64_matches_python_for_a_full_width_seed() {
    let s0 = crate::bignum::from_hex(
        "0x5af2212df9ff00948c75ab5dd21fd848b46439737e5d933bcfd8d969756856bc02e3958e46b5bcf40ca3fcddb92e53679533483377380407dcf3dc6ff054789afe42dc2a86035809b01cfebe65094fc0482fa407d4b9c7325fcefd3f57d536659061066d66e42dfde2b18c0a7a2504284fa20ddcad21d96dad9ecc2780ac3e6281807fcd5c9f5661ee4c35afa832166dd86565732537d00eed92358348ed4047e9f435bf7932b028b4ad79ac6940cb4638712597e2df7900776ebae55af4554437d8c87322f12fac6e423b0057667b183278315457b25c4e0ac8a3d47c28880f600ed55b98e00c8f28eb834d80b11184782c1448a921ee5c41083680cafdc916",
    )
    .expect("valid hex");
    const EXPECTED: &str = "0x1cb2d1fa5c04b9a6706d40b96ea226243dd435e8b7baa96e26f2b34efcc20a7d16e90988538bf439397d78a5da8ff441afbb7945bd6415a1da3fd465507dd3eb0c1ca3a5e591fe85a293449fb78a196327254c6e416790c77561f8bc9b6adfa712b2ecfa116566a4c31e9989209c097a9faf6235a15064cf392c0a08974e5092eff09e41a953b0791124d14bea87b979722316a1be04d6d25344ab841fd339a188262b957219cef0e2c7f0710f27e044192780f63416fe4535cb9875b3c0387956caa48126c667cae071b1a6f5256051045fb6ada221d1bde4f32b2c994ba9bda5b6bfbf52a545f35108e923c9c3b725fd1be9f06e3a26300cc8561d2d3cdf45";
    let got = sequential_squarings(&s0, 64).expect("chain");
    assert_eq!(to_hex(&got), EXPECTED);
}

/// Longer chain, still fast enough for a unit test (~2 ms), to catch an error that only
/// shows up once carries have propagated through many limbs.
#[test]
fn chain_of_1000_matches_python() {
    let mut s0 = ZERO;
    s0[0] = 0xDEAD_BEEF;
    const EXPECTED: &str = "0xbb6a0f5931e6c69679e02ad34a3182f2b4727f25c5427ae1fd3e617a170ecf27da2df5a9145b0e6495dd8a6e8cfb07e366efbdacefb537f9d5125d2b24864ffe287fb6215819b587df0e0e1487ba7a04b457cf4faae588dc2624cf3dcd73604bc47de01dc3afcec31733cb14b29ee051c1dee235844081714b34d6485dcd1f05989a2c836c5d7ad4cc7de85b2c1939abdc954f54a4de48782f77663039488482f6bfc4d8ac74f7dc7c291fd009fffcb838bb3d5e17fd5afe8a367ee9f89dde85a0463b9de4c206f07f887f7ab97c8cf1bbea6da98253b8354a9d8950352e130ef4939fd5c073863af91612b5f0814bd6c38a650c28a738190a81532f1e849a57";
    let got = sequential_squarings(&s0, 1000).expect("chain");
    assert_eq!(to_hex(&got), EXPECTED);
}

/// The chain is `S_0^(2^T)`, so `T` must be applied as a count of squarings, not as an
/// exponent. Running one step too few is the single easiest mistake here and it produces a
/// perfectly plausible-looking key.
#[test]
fn each_iteration_is_exactly_one_squaring() {
    let mut s0 = ZERO;
    s0[0] = 0xDEAD_BEEF;
    let mut manual = s0;
    for _ in 0..5 {
        manual = mul_mod_small(&manual, &manual, &N_PUB);
    }
    let got = sequential_squarings(&s0, 5).expect("chain");
    assert_eq!(got, manual);
    assert_ne!(got, sequential_squarings(&s0, 6).expect("chain"));
}

/// Rejections are errors, not silently-degraded chains.
#[test]
fn degenerate_seeds_are_rejected() {
    assert_eq!(sequential_squarings(&ZERO, 10), Err(VdfError::ZeroSeed));
    assert_eq!(sequential_squarings(&ONE, 0), Err(VdfError::ZeroIterations));
}

/// A seed that reduces to a non-zero value but still must be checked for coprimality.
///
/// We cannot construct a genuine zero divisor without the factors, so this asserts the
/// guard is reachable and correct on the cases we can build: zero, and a seed at or above
/// `N_pub` (which must be reduced before the gcd, or `gcd` would see a non-zero remainder).
#[test]
fn seeds_are_reduced_before_the_coprimality_check() {
    // N_pub itself reduces to zero.
    assert_eq!(sequential_squarings(&N_PUB, 10), Err(VdfError::ZeroSeed));
    // N_pub + 1 reduces to 1, which is coprime, so the chain runs.
    let mut n_plus_1 = N_PUB;
    crate::bignum::add_one(&mut n_plus_1);
    assert_eq!(sequential_squarings(&n_plus_1, 4).expect("chain"), {
        let mut one = ZERO;
        one[0] = 1;
        let mut v = one;
        for _ in 0..4 {
            v = mul_mod_small(&v, &v, &N_PUB);
        }
        v
    });
}

/// Determinism: the same seed and count must give the same answer, or the derived key
/// would depend on something other than the passcode.
#[test]
fn chain_is_deterministic() {
    let mut s0 = ZERO;
    s0[0] = 0x0123_4567_89AB_CDEF;
    let a = sequential_squarings(&s0, 128).expect("first");
    let b = sequential_squarings(&s0, 128).expect("second");
    assert_eq!(a, b);
}

/// `seed_from_k_int` is FR-4.4's `Read_U2048_LE(K_int) (mod N_pub)`: little-endian, and the
/// 64 input bytes occupy the *low* end.
#[test]
fn seed_from_k_int_is_little_endian_and_zero_extended() {
    let mut k = [0u8; 64];
    k[0] = 0x01;
    let s = seed_from_k_int(&k);
    assert_eq!(s[0], 1, "first byte of K_int is the least significant");
    assert!(s[1..].iter().all(|&l| l == 0));

    let mut k2 = [0u8; 64];
    k2[7] = 0x01;
    let s2 = seed_from_k_int(&k2);
    assert_eq!(
        s2[0], 0x0100_0000_0000_0000,
        "byte 7 lands at bit 56 of limb 0"
    );
}

/// The byte encodings round-trip, since `S_T` is fed to Blake2b as bytes.
#[test]
fn byte_encodings_roundtrip() {
    let mut s0 = ZERO;
    s0[0] = 0xDEAD_BEEF;
    let out = sequential_squarings(&s0, 16).expect("chain");
    let bytes = chain_to_le_bytes(&out);
    assert_eq!(bytes.len(), 256);
    assert_eq!(crate::bignum::from_le_bytes(&bytes), out);
    assert_eq!(n_pub_le_bytes().len(), 256);
    assert_eq!(crate::bignum::from_le_bytes(&n_pub_le_bytes()), N_PUB);
}

/// Measures the per-squaring cost. `#[ignore]`d so the gate stays fast; run explicitly:
///
/// ```text
/// cargo test --release -p holonomy-crypto -- --ignored --nocapture calibrate
/// ```
///
/// This is the precursor to the `vdf-calibrate` bin PROJECT.md §2.4 asks for. The point
/// of measuring rather than assuming is that PROJECT.md §1.1 measured 2,077 ns per
/// squaring for a different implementation; if this one is wildly different, the derived
/// iteration count `T` and therefore the whole unlock budget is wrong.
///
/// Measured on this host, release profile: **3,072 ns per squaring**. Same order as
/// §1.1's 2,077 ns, which is the sanity check that matters -- the CIOS arithmetic is not
/// quietly wrong. The gap is the release profile: §1.1 measured with `opt-level=3` and
/// `target-cpu=native`, this workspace builds `opt-level="z"` for the 2.5 MiB NFR-2.3
/// ceiling. Size optimisation costs roughly 50% of the throughput here, and the binary is
/// 377 KB against a 2.5 MiB budget, so there are ~2.1 MiB of headroom to trade back if
/// the unlock budget turns out to be the binding constraint on real hardware. That
/// decision belongs to Phase 2, which is where `T` is derived. See PROJECT.md §2.4.
#[test]
#[ignore = "timing, not correctness; run explicitly"]
fn calibrate_ns_per_squaring() {
    use std::time::Instant;
    let mut s0 = ZERO;
    s0[0] = 0xDEAD_BEEF;

    // Warm up so the first run is not measuring page faults and icache misses.
    let _ = sequential_squarings(&s0, 2000).expect("warmup");

    const ITERS: u64 = 20_000;
    let t0 = Instant::now();
    let out = sequential_squarings(&s0, ITERS).expect("chain");
    let elapsed = t0.elapsed();

    let ns = elapsed.as_nanos() as f64 / ITERS as f64;
    eprintln!("calibration: {ITERS} squarings in {elapsed:?}");
    eprintln!("calibration: {ns:.1} ns per squaring");
    eprintln!(
        "calibration: implied T for a 1000 ms budget = {}",
        (1_000_000_000.0 / ns) as u64
    );
    eprintln!("calibration: result = {}", to_hex(&out));
}
