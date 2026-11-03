//! Bignum tests.
//!
//! The big vectors below were produced by CPython's arbitrary-precision integers, an
//! implementation that shares no code with this one. That is the point: the expected
//! values are not this crate's own output checked against itself.
//!
//! The small-modulus tests are differential in the other direction: they compare
//! [`pow_mod`] against a naive multiply-and-reduce, so a bug that affects both the
//! Montgomery path and the naive path would have to be made twice, in two different
//! styles of code, to survive.

use super::*;

/// A 2048-bit prime, chosen arbitrarily. Its only job is to be odd and prime.
const N_HEX: &str = "0xce64e5c5a5df862752764e84156a79106add0f84ec7e2915b221cc9ab15eab9bb0600e03ee9ffd4d97a6512810c2cb701ec6f825523a12c90e95b7dfe980ce0099cac628858b676a9e02f7aef091ed6ca04a7068c6f9fd8b34c27f70757592cf8d8eaf10b9e65ade916c0d2d877464f74088d0aca3f59a692a708acf97786bce07511fa25e777aea516c62995fa5e2b8d45155cf77ae345c30d056be3fe11ef0c0598e401dbddc4a13fd840980ad91e9c3fc0b944a9f9228b75a31496fae4d715023580d4ce13418f392ed6f785827a01a3e829bd3aa5fc20fbd9f5c41a7e868b60226b4861237611689172e32d64f8fe5d7d9c862252ae69eadb623ea38ab93";
const A_HEX: &str = "0x5af2212df9ff00948c75ab5dd21fd848b46439737e5d933bcfd8d969756856bc02e3958e46b5bcf40ca3fcddb92e53679533483377380407dcf3dc6ff054789afe42dc2a86035809b01cfebe65094fc0482fa407d4b9c7325fcefd3f57d536659061066d66e42dfde2b18c0a7a2504284fa20ddcad21d96dad9ecc2780ac3e6281807fcd5c9f5661ee4c35afa832166dd86565732537d00eed92358348ed4047e9f435bf7932b028b4ad79ac6940cb4638712597e2df7900776ebae55af4554437d8c87322f12fac6e423b0057667b183278315457b25c4e0ac8a3d47c28880f600ed55b98e00c8f28eb834d80b11184782c1448a921ee5c41083680cafdc916";
const B_HEX: &str = "0x2937474d9ca8f5bac7de65fcf2ee275f33b793c4dd0f845d014c671fd95d14a1a5efbf0f16011b81f1f7ba71cd38255bee5fbf35614b78401a4f9dc5cf123f4218ae5477f704e7194cf9c5f0d94793802129d9543d7ce13ab742d1e62d23eebe066cc2dd9ce9ee43a02ac759538dc56628716629c039c06d7d9d4de79cd31b6ad178d237b314614012b18b60da014ef1f1a34a0a5bc64f79c5c4b2166a295b55bec13b9a7c4fda689f95a59a95e084369aa8044e6689f389c1dcf75a1f7f13e6332bea345f39aba7a7fe963ae1ad234a7b3194d1ecccc065f3732251d40dc1d79057a0d5f3646a16b41915d66980aa908082670c2436f0c8c01580a9f3840f0f";

fn h(s: &str) -> U2048 {
    from_hex(s).expect("valid hex vector")
}

#[test]
fn hex_roundtrips() {
    let n = h(N_HEX);
    assert_eq!(to_hex(&n), N_HEX);
    assert_eq!(bit_len(&n), 2048);
    // Whitespace, underscores and a missing 0x prefix must all parse.
    let mangled = N_HEX.trim_start_matches("0x");
    assert_eq!(from_hex(mangled), Some(n));
    // A short string is left-padded to the full 32 limbs, so it keeps its true value:
    // "0x1" is 1, stored in limb 0.
    assert_eq!(from_hex("0x1"), Some(ONE));
    assert_eq!(from_hex("0x00_00 01"), Some(ONE));
    assert_eq!(from_hex("0xzz"), None);
    assert_eq!(from_hex(""), None);
}

#[test]
fn le_bytes_roundtrip() {
    let n = h(N_HEX);
    let bytes = to_le_bytes(&n);
    assert_eq!(bytes.len(), 256);
    assert_eq!(from_le_bytes(&bytes), n);

    // FR-4.4 specifies `Read_U2048_LE`, so limb 0 must be the low 8 bytes first.
    let mut one = ZERO;
    one[0] = 0x0102_0304_0506_0708;
    let b = to_le_bytes(&one);
    assert_eq!(&b[0..8], &[0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]);
    assert!(b[8..].iter().all(|&x| x == 0));
}

#[test]
fn comparison_and_bits() {
    assert_eq!(cmp(&ZERO, &ZERO), Ordering::Equal);
    assert_eq!(cmp(&ONE, &ZERO), Ordering::Greater);
    assert_eq!(cmp(&ZERO, &ONE), Ordering::Less);

    assert_eq!(bit_len(&ZERO), 0);
    assert_eq!(bit_len(&ONE), 1);
    // The top bit of the top limb, i.e. a genuinely 2048-bit value.
    let mut top = ZERO;
    top[LIMBS - 1] = 1 << 63;
    assert_eq!(bit_len(&top), 2048);
    // `top[LIMBS-1] = 1` would be 2^1984, not 2048 bits. Pinned because that is exactly
    // the mistake this test was originally making.
    let mut high_limb_low = ZERO;
    high_limb_low[LIMBS - 1] = 1;
    assert_eq!(bit_len(&high_limb_low), 1985);

    assert!(is_odd(&ONE) && !is_odd(&ZERO));
    assert!(bit(&top, 2047) && !bit(&top, 2046));
    assert!(!bit(&top, 9999), "out-of-range bit must read as zero");

    let mut a = [0xFFFF_FFFF_FFFF_FFFFu64; LIMBS];
    assert_eq!(shl1(&mut a), 1, "carry out of the top");
    // Limb 0 loses its top bit with nothing to carry in; every later limb receives a
    // carry of 1 and so comes out unchanged.
    assert_eq!(a[0], u64::MAX - 1);
    assert!(a[1..].iter().all(|&l| l == u64::MAX));
    let mut b = [0u64; LIMBS];
    b[0] = 2;
    assert_eq!(shr1(&mut b), 0);
    assert_eq!(b[0], 1);
}

#[test]
fn add_and_sub_are_inverse() {
    let a = h(A_HEX);
    let b = h(B_HEX);
    let mut t = a;
    let carry = add_carry(&mut t, &b);
    assert_eq!(carry, 0, "sum of two <2^2047 values must not overflow");
    let borrow = sub_borrow(&mut t, &b);
    assert_eq!(borrow, 0);
    assert_eq!(t, a);

    // Underflow is reported, not silently wrapped.
    let mut small = ONE;
    assert_eq!(sub_borrow(&mut small, &h(N_HEX)), 1);
}

#[test]
fn rem_matches_python() {
    let n = h(N_HEX);

    // A value just below n is returned unchanged.
    let a = h(A_HEX);
    assert!(cmp(&a, &n) == Ordering::Less);
    assert_eq!(rem(&a, &n), a);

    // n + 12345, which still fits in 2048 bits and reduces to 12345.
    let mut over = n;
    let mut k = ZERO;
    k[0] = 12345;
    let _ = add_carry(&mut over, &k);
    let mut expect = ZERO;
    expect[0] = 12345;
    assert_eq!(rem(&over, &n), expect);

    // The worst case for the accumulator: 2^2048 - 1 against a modulus whose top bit is
    // set. Every one of the 2048 steps reduces, so the intermediate remainder sits at
    // nearly 2^2048 and needs the extra limb.
    let mut all_ones = ZERO;
    for limb in all_ones.iter_mut() {
        *limb = u64::MAX;
    }
    const EXPECTED: &str = "0x319b1a3a5a2079d8ad89b17bea9586ef9522f07b1381d6ea4dde33654ea154644f9ff1fc116002b26859aed7ef3d348fe13907daadc5ed36f16a4820167f31ff663539d77a74989561fd08510f6e12935fb58f9739060274cb3d808f8a8a6d30727150ef4619a5216e93f2d2788b9b08bf772f535c0a6596d58f753068879431f8aee05da1888515ae939d66a05a1d472baeaa308851cba3cf2fa941c01ee10f3fa671bfe24223b5ec027bf67f526e163c03f46bb5606dd748a5ceb69051b28eafdca7f2b31ecbe70c6d129087a7d85fe5c17d642c55a03df04260a3be58179749fdd94b79edc89ee976e8d1cd29b0701a2826379ddad519615249dc15c7546c";
    assert_eq!(to_hex(&rem(&all_ones, &n)), EXPECTED);
}

#[test]
fn mont_n0inv_is_the_negated_inverse() {
    let n = h(N_HEX);
    let n0inv = mont_n0inv(&n);
    // n * n0inv == -1 (mod 2^64)
    let product = (n[0] as u128) * (n0inv as u128);
    assert_eq!((product & u128::from(u64::MAX)) as u64, u64::MAX);
}

#[test]
fn mont_r2_and_n0inv_match_python() {
    let n = h(N_HEX);

    // Previously this test derived r2 from mont_mul and so could not catch a wrong r2.
    // Pinned against the oracle instead.
    const R_MOD_N: &str = "0x319b1a3a5a2079d8ad89b17bea9586ef9522f07b1381d6ea4dde33654ea154644f9ff1fc116002b26859aed7ef3d348fe13907daadc5ed36f16a4820167f31ff663539d77a74989561fd08510f6e12935fb58f9739060274cb3d808f8a8a6d30727150ef4619a5216e93f2d2788b9b08bf772f535c0a6596d58f753068879431f8aee05da1888515ae939d66a05a1d472baeaa308851cba3cf2fa941c01ee10f3fa671bfe24223b5ec027bf67f526e163c03f46bb5606dd748a5ceb69051b28eafdca7f2b31ecbe70c6d129087a7d85fe5c17d642c55a03df04260a3be58179749fdd94b79edc89ee976e8d1cd29b0701a2826379ddad519615249dc15c7546d";
    const R2_MOD_N: &str = "0x45238c22bc738affec511926f6721f42122822f03c7deb7670278299fe3466fc37c8eced8e751c0badeece20caff4c2e9944d9e5929b0d1690071bbe6e3cc0ca3f65241202693e917b6282c946c6235bed40a613ae4ddf52e220f4c871044191f2cc7202faabdd1f49dbfc8df7d0bc179bff7e020abcb71a29ed6a05756824c5829ed936db26f82461d37015932f3790f21e8ee364c0847f2621fa4482807b6f7dea76fa7d8779c34d1dc81d15258e10e85be542ca244e8f33868fd13204d57a734867e30bbd13dd05894034f1e43ce7691518b47c24970198b341fdde5c1bc401b7969893588559724f8db6ab9a975b43bdbb354085945d6fa1d60c8f034b62";

    assert_eq!(mont_r2(&n), h(R2_MOD_N));
    assert_eq!(mont_n0inv(&n), 0x4d48_b4d4_9771_d565);
    // R mod n follows from r2 by one Montgomery reduction, and that is the check that
    // actually exercises the reduction path.
    assert_eq!(from_mont(&mont_r2(&n), &n, mont_n0inv(&n)), h(R_MOD_N));
}

#[test]
fn montgomery_roundtrips() {
    let n = h(N_HEX);
    let n0inv = mont_n0inv(&n);
    let r2 = mont_r2(&n);

    for x in [h(A_HEX), ONE, h(B_HEX), n_minus_one(&n)] {
        let m = to_mont(&x, &n, n0inv, &r2);
        assert!(cmp(&m, &n) == Ordering::Less);
        assert_eq!(
            from_mont(&m, &n, n0inv),
            x,
            "to_mont/from_mont must round-trip"
        );
    }
}

#[test]
fn montgomery_product_matches_python() {
    let n = h(N_HEX);
    let n0inv = mont_n0inv(&n);
    let r2 = mont_r2(&n);
    let a = h(A_HEX);
    let b = h(B_HEX);

    // CPython: (a * b) mod n
    const EXPECTED: &str = "0x2bfded824f7aaa412f6b2ab2a1e896a679151a8b77dd41223c71edd7f7c3fca43224aac91a7238c537307fc611837e01157b98100bbc8ac0d8280f856fecf650372c2e418e4e9b2438f2219b1e04a56ab8b18645b2e5141aa176a2cd2832ee38c57be3d0c9b53b1cb29ea6332c8519ffd70c23294ab9f12ef5c795987955dfb3e25a90d857768c96d60df7b53872bf5f5a70f56185a0fdc9db570dc86e55f41634135ddf2119c8ac2d9337aa2ba92204faecb3f7552d543d29acc303ae50a174be7fecf5a16b51f30215714130586a9485fcf6dba8de71c11747d325d5a3862113045f08a074889610f1a7194e90679ec7b84ca89a0bb9e9ad990bf476f80b38";
    let product = from_mont(
        &mont_mul(
            &to_mont(&a, &n, n0inv, &r2),
            &to_mont(&b, &n, n0inv, &r2),
            &n,
            n0inv,
        ),
        &n,
        n0inv,
    );
    assert_eq!(to_hex(&product), EXPECTED);
}

#[test]
fn pow_mod_matches_python() {
    let n = h(N_HEX);
    // 2^255 is below n, so it cannot have been reduced.
    let mut two = ZERO;
    two[0] = 2;
    let mut exp255 = ZERO;
    exp255[0] = 255;
    // Compared as a U2048: `to_hex` is fixed-width at 512 characters, so comparing its
    // output against a 64-digit literal would fail on width even when the value is right.
    assert_eq!(
        pow_mod(&two, &exp255, &n),
        h("0x8000000000000000000000000000000000000000000000000000000000000000")
    );

    let mut exp65537 = ZERO;
    exp65537[0] = 65537;
    const EXPECTED_65537: &str = "0x2646db6e3451d69f8d151c57ff4ee18eb5157e37c465cb2a2db49812fff4155f0faee132c022e04204257495b554109d846afe859284867617c592a7c009cf4a23d6241e84f6210e1338fb95fc0dcaf0d84f5beac505d867b7362815c9924865267787346d4aceba4ee2e3790e2ce79eda9374a38bf8a5bbfc455165a0393f8d938016e229c355bb960c9cf8f487dd1cffe045b2b93433b61cb8e99b71e3aeeb1fd4f5670a7a2408793d2eaeaab2fe98def14e4736f06cee5b13ec14a353fbbc6e2d8b7d3c08cf0916b2d5598f60099c89836cf4d2587f7173af9efaa4448b6321f512e89a3ad10af4c292fa7d8cbcd301e3e6cec1f45ec0efd529a393fb3972";
    assert_eq!(pow_mod(&two, &exp65537, &n), h(EXPECTED_65537));
}

#[test]
fn fermat_holds_for_the_test_modulus() {
    let n = h(N_HEX);
    let mut n_minus_1 = n;
    sub_one(&mut n_minus_1);
    for x in [h(A_HEX), h(B_HEX), TWO_SMALL] {
        assert_eq!(
            pow_mod(&x, &n_minus_1, &n),
            ONE,
            "Fermat: a^(n-1) = 1 mod n"
        );
    }
}

#[test]
fn pow_mod_agrees_with_naive_repeated_squaring() {
    // Differential against the naive path on a 64-bit modulus.
    const M: &str = "0xfa0007ca3f843011";
    const SMALL_HEX: &str = "0x1b9bf0f5646e9ce8";
    let m = h(M);

    let base = h(A_HEX);
    let b = h(B_HEX);
    let mut exp = ZERO;
    exp[0] = 0x0123_4567;

    let fast = pow_mod(&base, &exp, &m);

    // Naive: square-and-multiply using mul_mod_small.
    let mut acc = ONE;
    let mut cur = rem(&base, &m);
    for i in 0..bit_len(&exp) {
        if bit(&exp, i) {
            acc = mul_mod_small(&acc, &cur, &m);
        }
        cur = mul_mod_small(&cur, &cur, &m);
    }

    assert_eq!(acc, fast);
    // And pin against the oracle: MULMOD is (a mod m) * (b mod m) mod m, a product and
    // not a square.
    let check = mul_mod_small(&rem(&base, &m), &rem(&b, &m), &m);
    assert_eq!(check, h(SMALL_HEX));
}

#[test]
fn gcd_matches_python() {
    let n = h(N_HEX);
    let a = h(A_HEX);
    let b = h(B_HEX);
    // Oracle reported 1 for both of these.
    assert_eq!(gcd(a, b), ONE);
    assert_eq!(gcd(a, n), ONE);
    // And a case built to exercise the shift-heavy path.
    let mut d = ZERO;
    d[LIMBS - 1] = 0xF000_0000_0000_0000;
    let mut x = d;
    let mut y = d;
    for _ in 0..3 {
        shl1(&mut x);
        shl1(&mut y);
    }
    // Consecutive integers are coprime, which is the case that used to hang: the
    // binary-GCD loop could zero `a`, and the next iteration would then swap `b` to zero
    // and subtract zero from `a` forever.
    let _ = add_carry(&mut y, &ONE);
    assert_eq!(gcd(x, y), ONE);
    assert_eq!(gcd(x, x), x, "identical inputs take the other exit");
    assert_eq!(gcd(ZERO, x), x);
    assert_eq!(gcd(x, ZERO), x);
    assert_eq!(gcd(ZERO, ZERO), ZERO);
}

#[test]
fn mod_u64_agrees_with_rem() {
    let n = h(N_HEX);
    let a = h(A_HEX);
    for m in [3u64, 7, 65_537, 1_000_000_007, u64::MAX] {
        let native = mod_u64(&a, m);
        let via_rem = {
            let mut small = ZERO;
            small[0] = m;
            mod_u64(&rem(&a, &small), m)
        };
        assert_eq!(native, via_rem, "m = {m}");
    }
    assert_eq!(mod_u64(&n, 2), 1, "the test modulus is odd");
}

const TWO_SMALL: U2048 = {
    let mut v = ZERO;
    v[0] = 2;
    v
};

#[test]
fn shl_and_shr_agree_with_the_single_bit_primitives() {
    // A base with the top eight limbs cleared, so shifting left by up to 512 bits cannot
    // discard anything. That matters: `shl(n, 1)` on a 2048-bit value whose top bit is
    // set loses that bit, so shifting up and back down is NOT the identity -- it equals
    // `n >> 1` only when nothing was lost.
    let mut base = h(N_HEX);
    for limb in base.iter_mut().skip(LIMBS - 8) {
        *limb = 0;
    }
    assert_eq!(bit_len(&base), 1536);

    for bits in [0usize, 1, 7, 63, 64, 65, 127, 511] {
        let mut want_right = base;
        for _ in 0..bits {
            shr1(&mut want_right);
        }
        let mut got_right = base;
        shr(&mut got_right, bits);
        assert_eq!(to_hex(&got_right), to_hex(&want_right), "shr by {bits}");

        let mut want_left = base;
        for _ in 0..bits {
            shl1(&mut want_left);
        }
        let mut got_left = base;
        shl(&mut got_left, bits);
        assert_eq!(to_hex(&got_left), to_hex(&want_left), "shl by {bits}");
    }

    // Shifting a whole width clears the value rather than panicking.
    let mut t = base;
    shl(&mut t, 2048);
    assert_eq!(t, ZERO);
    shr(&mut t, 4096);
    assert_eq!(t, ZERO);

    let mut small = ONE;
    shl(&mut small, 5);
    assert_eq!(small[0], 32);
    shr(&mut small, 5);
    assert_eq!(small, ONE);
}

fn n_minus_one(n: &U2048) -> U2048 {
    let mut t = *n;
    sub_one(&mut t);
    t
}
