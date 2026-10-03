//! Fixed-width 2048-bit unsigned integer arithmetic and Montgomery multiplication.
//!
//! This exists because there is no crate to reach for. The VDF is the load-bearing part
//! of the unlock path and the whole point is that it runs sequentially on one core, so
//! its inner loop has to be code we can read and measure rather than a dependency whose
//! cost we cannot account for. Everything here is fixed-width and allocation-free:
//! `U2048` is `[u64; 32]`, and no function in this module touches the heap.
//!
//! # Why Montgomery form
//!
//! The Wesolowski chain is `S_i = S_{i-1}^2 mod N`, repeated `T` times. What makes it
//! *non-parallelizable* is that each step depends on the previous one, so the arithmetic
//! has to be as fast as possible or the time-lock budget is dominated by the modexp
//! overhead rather than by the chain. Montgomery multiplication replaces a division by
//! `N` at every step with a multiplication by a precomputed constant, which on this
//! target is the difference between a usable and an unusable unlock.
//!
//! # Representation
//!
//! [`mont_mul`] keeps values in Montgomery form: a value `x` is stored as `x * R mod N`
//! where `R = 2^(64 * LIMBS)`. That means `mont_mul(a, b)` computes the Montgomery
//! product `a * b * R^-1 mod N`, which is exactly the stored form of `a * b`. Squaring
//! in Montgomery form is therefore `mont_mul(a, a)` -- no conversion per step.

use core::cmp::Ordering;

/// Number of 64-bit limbs in a 2048-bit integer.
pub const LIMBS: usize = 32;

/// A 2048-bit unsigned integer, little-endian by limb.
pub type U2048 = [u64; LIMBS];

/// The zero value.
pub const ZERO: U2048 = [0; LIMBS];

/// The one value.
pub const ONE: U2048 = {
    let mut v = [0u64; LIMBS];
    v[0] = 1;
    v
};

/// Order two values by limb, most significant first.
#[inline]
pub fn cmp(a: &U2048, b: &U2048) -> Ordering {
    for i in (0..LIMBS).rev() {
        match a[i].cmp(&b[i]) {
            Ordering::Equal => continue,
            other => return other,
        }
    }
    Ordering::Equal
}

/// `a += b`, returning the carry out of the top limb.
#[inline]
pub fn add_carry(a: &mut U2048, b: &U2048) -> u64 {
    let mut carry = 0u64;
    for i in 0..LIMBS {
        let (s1, c1) = a[i].overflowing_add(b[i]);
        let (s2, c2) = s1.overflowing_add(carry);
        a[i] = s2;
        carry = u64::from(c1) + u64::from(c2);
    }
    carry
}

/// `a -= b`, returning the borrow out of the top limb.
#[inline]
pub fn sub_borrow(a: &mut U2048, b: &U2048) -> u64 {
    let mut borrow = 0u64;
    for i in 0..LIMBS {
        let (d1, b1) = a[i].overflowing_sub(b[i]);
        let (d2, b2) = d1.overflowing_sub(borrow);
        a[i] = d2;
        borrow = u64::from(b1) + u64::from(b2);
    }
    borrow
}

/// True when every limb is zero.
#[inline]
pub fn is_zero(a: &U2048) -> bool {
    a.iter().all(|&l| l == 0)
}

/// True when the top bit of limb 0 is set, i.e. the value is odd.
#[inline]
pub fn is_odd(a: &U2048) -> bool {
    a[0] & 1 == 1
}

/// Bit `i` of the value. Out-of-range indices read as zero.
#[inline]
pub fn bit(a: &U2048, i: usize) -> bool {
    if i >= LIMBS * 64 {
        return false;
    }
    (a[i / 64] >> (i % 64)) & 1 == 1
}

/// Bit length: the index of the highest set bit plus one. Zero has bit length 0.
pub fn bit_len(a: &U2048) -> usize {
    for i in (0..LIMBS).rev() {
        if a[i] != 0 {
            return i * 64 + (64 - a[i].leading_zeros() as usize);
        }
    }
    0
}

/// Number of trailing zero bits in the whole value.
///
/// Not `a[0].trailing_zeros()`: that is only correct when limb 0 is non-zero, and
/// returns 64 for a zero limb when the value may have hundreds more trailing zeros. See
/// the `gcd` comment for what that costs.
pub fn trailing_zeros(a: &U2048) -> usize {
    for (i, limb) in a.iter().enumerate() {
        if *limb != 0 {
            return i * 64 + (limb.trailing_zeros() as usize);
        }
    }
    LIMBS * 64
}

/// `a <<= 1`, returning the bit shifted out of the top.
#[inline]
pub fn shl1(a: &mut U2048) -> u64 {
    let mut carry = 0u64;
    for limb in a.iter_mut() {
        let next = *limb >> 63;
        *limb = (*limb << 1) | carry;
        carry = next;
    }
    carry
}

/// `a >>= 1`, returning the bit shifted out of the bottom.
#[inline]
pub fn shr1(a: &mut U2048) -> u64 {
    let mut carry = 0u64;
    for i in (0..LIMBS).rev() {
        let next = a[i] & 1;
        a[i] = (a[i] >> 1) | (carry << 63);
        carry = next;
    }
    carry
}

/// `a <<= bits`, discarding whatever is shifted out.
///
/// Arrays have no `<<=`, and `gcd` needs whole-value shifts, so this is written out
/// rather than looped over [`shl1`].
pub fn shl(a: &mut U2048, bits: usize) {
    if bits >= LIMBS * 64 {
        *a = ZERO;
        return;
    }
    let limbs = bits / 64;
    let rem = bits % 64;
    if rem == 0 {
        for i in (limbs..LIMBS).rev() {
            a[i] = a[i - limbs];
        }
    } else {
        for i in (limbs..LIMBS).rev() {
            let lo = a[i - limbs];
            let hi = if i > limbs { a[i - limbs - 1] } else { 0 };
            a[i] = (lo << rem) | (hi >> (64 - rem));
        }
    }
    for limb in a.iter_mut().take(limbs) {
        *limb = 0;
    }
}

/// `a >>= bits`.
///
/// See [`shl`] for why this is spelled out rather than looped.
pub fn shr(a: &mut U2048, bits: usize) {
    if bits >= LIMBS * 64 {
        *a = ZERO;
        return;
    }
    let limbs = bits / 64;
    let rem = bits % 64;
    if rem == 0 {
        for i in 0..(LIMBS - limbs) {
            a[i] = a[i + limbs];
        }
    } else {
        for i in 0..(LIMBS - limbs) {
            let lo = a[i + limbs];
            let hi = if i + limbs + 1 < LIMBS {
                a[i + limbs + 1]
            } else {
                0
            };
            a[i] = (lo >> rem) | (hi << (64 - rem));
        }
    }
    for limb in a.iter_mut().skip(LIMBS - limbs) {
        *limb = 0;
    }
}

/// `a += 1`, wrapping on overflow. Used by `gcd`, where wrapping is the desired
/// behaviour and carries past the width are meaningless.
#[inline]
pub fn add_one(a: &mut U2048) -> u64 {
    for limb in a.iter_mut() {
        let (v, over) = limb.overflowing_add(1);
        *limb = v;
        if !over {
            return 0;
        }
    }
    1
}

/// `a -= 1`, wrapping on underflow. Used by `gcd`.
#[inline]
pub fn sub_one(a: &mut U2048) -> u64 {
    for limb in a.iter_mut() {
        let (v, under) = limb.overflowing_sub(1);
        *limb = v;
        if !under {
            return 0;
        }
    }
    1
}

/// Bitwise AND.
pub fn bitand(a: &U2048, b: &U2048) -> U2048 {
    let mut out = ZERO;
    for i in 0..LIMBS {
        out[i] = a[i] & b[i];
    }
    out
}

/// Bitwise OR.
pub fn bitor(a: &U2048, b: &U2048) -> U2048 {
    let mut out = ZERO;
    for i in 0..LIMBS {
        out[i] = a[i] | b[i];
    }
    out
}

/// A 2049-bit accumulator, one limb wider than [`U2048`].
///
/// Needed wherever the running value is `x mod m` with `x < m`, and is then doubled.
/// Since `m` may be almost `2^2048`, `2x` does not fit in 2048 bits, and a 2048-bit
/// accumulator silently drops the top bit and produces a wrong answer. This is not a
/// theoretical concern: `mont_r2` returns zero instead of `R^2 mod n` without the extra
/// limb.
///
/// The invariant maintained by [`ext_reduce`] is `r < m`, which guarantees that a single
/// conditional subtraction after doubling is always enough.
type Ext = [u64; LIMBS + 1];

/// `r <<= 1` over the full width.
fn ext_double(r: &mut Ext) {
    let mut carry = 0u64;
    for limb in r.iter_mut() {
        let next = *limb >> 63;
        *limb = (*limb << 1) | carry;
        carry = next;
    }
}

/// Subtract `m` from `r` if `r >= m`. Restores the `r < m` invariant.
fn ext_reduce(r: &mut Ext, m: &U2048) {
    // Compare, with m's implicit top limb treated as zero.
    let mut geq = true;
    for k in (0..=LIMBS).rev() {
        let mv = if k == LIMBS { 0 } else { m[k] };
        match r[k].cmp(&mv) {
            Ordering::Equal => {}
            Ordering::Greater => {
                geq = true;
                break;
            }
            Ordering::Less => {
                geq = false;
                break;
            }
        }
    }
    if !geq {
        return;
    }
    let mut borrow = 0u64;
    for k in 0..LIMBS {
        let (d1, b1) = r[k].overflowing_sub(m[k]);
        let (d2, b2) = d1.overflowing_sub(borrow);
        r[k] = d2;
        borrow = u64::from(b1) + u64::from(b2);
    }
    let (top, under) = r[LIMBS].overflowing_sub(borrow);
    r[LIMBS] = top;
    debug_assert!(!under, "reduction went negative");
}

/// Narrow an [`Ext`] to a [`U2048`]. The caller must already have reduced.
fn ext_narrow(r: &Ext) -> U2048 {
    debug_assert_eq!(r[LIMBS], 0, "ext accumulator still has a 2049th bit set");
    let mut out = ZERO;
    out.copy_from_slice(&r[..LIMBS]);
    out
}

/// `a mod m`.
///
/// Bitwise long division: shift the dividend in one bit at a time, reducing against `m`
/// each step.
///
/// The obvious alternative -- subtract a shifted copy of `m` per bit position -- is
/// wrong unless you also divide by the quotient, since `r` may exceed twice the aligned
/// divisor.
pub fn rem(a: &U2048, m: &U2048) -> U2048 {
    assert!(!is_zero(m), "rem by zero");
    let mut r = [0u64; LIMBS + 1];
    for i in (0..bit_len(a)).rev() {
        ext_double(&mut r);
        if bit(a, i) {
            r[0] |= 1;
        }
        ext_reduce(&mut r, m);
    }
    ext_narrow(&r)
}

/// `a mod m` for a modulus that fits in one limb, via the native 128-bit path.
///
/// This is the path generation uses. [`rem`] costs `O(bits * limbs)` and would dominate
/// a trial-division sieve; this costs `O(bits)`.
pub fn mod_u64(a: &U2048, m: u64) -> u64 {
    assert!(m != 0, "mod by zero");
    let mut r = 0u64;
    for i in (0..LIMBS).rev() {
        // `r < m < 2^64`, so `r << 64 | a[i]` cannot overflow a u128: the largest value
        // is (2^64 - 1) << 64 + (2^64 - 1) = 2^128 - 1.
        let wide = ((r as u128) << 64) | (a[i] as u128);
        r = (wide % (m as u128)) as u64;
    }
    r
}

/// `(a * b) mod m`, schoolbook shift-and-add.
///
/// The caller must keep `a * b` inside 2048 bits; this panics rather than silently
/// truncating if it does not. Only for small operands (trial division, and the naive
/// reference path the differential tests compare against). The VDF uses [`mont_mul`].
pub fn mul_mod_small(a: &U2048, b: &U2048, m: &U2048) -> U2048 {
    let bits = LIMBS * 64;
    let blen = bit_len(b);
    let mut acc = ZERO;
    for i in 0..bits {
        if !bit(a, i) {
            continue;
        }
        assert!(
            i + blen <= bits,
            "mul_mod_small: a * b does not fit in 2048 bits"
        );
        // acc += b << i
        let limb_off = i / 64;
        let bit_off = i % 64;
        let mut carry = 0u128;
        for (j, &bj) in b.iter().enumerate().take(LIMBS - limb_off) {
            let dst = limb_off + j;
            // The limb of (b << i) landing at `dst`, split into the part that belongs
            // here and the part that carries into `dst + 1`.
            let lo = if bit_off == 0 { bj } else { bj << bit_off };
            let hi = if bit_off == 0 {
                0
            } else if j + 1 < LIMBS {
                bj >> (64 - bit_off)
            } else {
                0
            };
            let cur = acc[dst] as u128 + lo as u128 + carry;
            acc[dst] = cur as u64;
            carry = (cur >> 64) + hi as u128;
        }
    }
    rem(&acc, m)
}

/// Greatest common divisor, binary GCD.
///
/// Binary rather than Euclidean-with-division because [`rem`] here is subtraction-based
/// and would be `O(n)` per step, making GCD `O(n^2)`.
pub fn gcd(mut a: U2048, mut b: U2048) -> U2048 {
    if is_zero(&a) {
        return b;
    }
    if is_zero(&b) {
        return a;
    }
    // Remove common powers of two. This must use the *whole-value* trailing-zero count.
    // Reading `a[0].trailing_zeros()` alone caps the shift at 64 bits, so a value whose
    // low limb happens to be zero stays even; binary GCD then has an even operand, every
    // subtraction leaves the result odd, the shift does nothing, and the loop grinds
    // down one subtraction at a time. On a 2048-bit pair that is ~2^1984 iterations.
    let sa = trailing_zeros(&a);
    let sb = trailing_zeros(&b);
    let shift = sa.min(sb);
    shr(&mut a, sa);
    shr(&mut b, sb);
    loop {
        // Checked at the top, not only after the subtraction: the shift below can zero
        // `a`, and if that went unchecked the next iteration would swap `b` to zero and
        // then subtract zero from `a` forever.
        if is_zero(&a) {
            break;
        }
        if cmp(&a, &b) == Ordering::Less {
            core::mem::swap(&mut a, &mut b);
        }
        let _ = sub_borrow(&mut a, &b);
        if is_zero(&a) {
            break;
        }
        let tz = trailing_zeros(&a);
        shr(&mut a, tz);
    }
    let mut g = b;
    for _ in 0..shift {
        shl1(&mut g);
    }
    g
}

/// `n' = -n^-1 mod 2^64`, the Montgomery constant for `n`.
///
/// Newton's iteration doubles the number of correct bits each step, so six rounds
/// reach 64 from one correct bit. `n` must be odd, which holds for every prime modulus.
pub fn mont_n0inv(n: &U2048) -> u64 {
    assert!(is_odd(n), "Montgomery modulus must be odd");
    // Start with an inverse correct modulo 2 (n is odd, so n^(-1) = 1 mod 2).
    let mut inv: u64 = 1;
    for _ in 0..6 {
        inv = inv.wrapping_mul(2u64.wrapping_sub(n[0].wrapping_mul(inv)));
    }
    inv.wrapping_neg()
}

/// `R^2 mod n`, needed to convert into Montgomery form.
///
/// Computed by repeated doubling: `R^2 mod n` is what you get after `2*LIMBS*64`
/// doublings of one, each reduced. Slow (O(bits^2)) but run once at open, not in the
/// chain.
pub fn mont_r2(n: &U2048) -> U2048 {
    assert!(is_odd(n), "Montgomery modulus must be odd");
    let mut r = [0u64; LIMBS + 1];
    r[0] = 1;
    // R^2 = 2^(2 * 64 * LIMBS). Doubling with a reduction each step keeps `r < n`, and
    // the wide accumulator keeps the 2049th bit that `n` being nearly 2^2048 requires.
    for _ in 0..(2 * 64 * LIMBS) {
        ext_double(&mut r);
        ext_reduce(&mut r, n);
    }
    ext_narrow(&r)
}

/// Reduce `x` into `[0, n)`, assuming `x < 2n`.
#[inline]
pub fn reduce_once(x: &mut U2048, n: &U2048) {
    if cmp(x, n) != Ordering::Less {
        let _ = sub_borrow(x, n);
    }
}

/// Montgomery multiplication: returns `a * b * R^-1 mod n`, in `[0, n)`.
///
/// Coarsely Integrated Operand Scanning (CIOS). `n` must be odd; `n0inv` must be
/// [`mont_n0inv`] of `n`. Both `a` and `b` must already be in `[0, n)`.
pub fn mont_mul(a: &U2048, b: &U2048, n: &U2048, n0inv: u64) -> U2048 {
    debug_assert!(cmp(a, n) == Ordering::Less && cmp(b, n) == Ordering::Less);

    // t holds k+2 limbs so the final carry has somewhere to go.
    let mut t = [0u64; LIMBS + 2];

    for &ai in a.iter() {
        // t += a[i] * b
        let mut carry = 0u128;
        for j in 0..LIMBS {
            let cur = t[j] as u128 + (ai as u128) * (b[j] as u128) + carry;
            t[j] = cur as u64;
            carry = cur >> 64;
        }
        let cur = t[LIMBS] as u128 + carry;
        t[LIMBS] = cur as u64;
        t[LIMBS + 1] = (cur >> 64) as u64;

        // m chosen so that t[0] + m*n[0] == 0 (mod 2^64)
        let m = t[0].wrapping_mul(n0inv);

        // t += m * n, writing one limb lower as we go. The j = 0 term is handled on its
        // own because its result would land in t[-1]: by the choice of `m` that term is
        // exactly zero, which is what frees the shift.
        let cur = t[0] as u128 + (m as u128) * (n[0] as u128);
        debug_assert_eq!(cur as u64, 0, "m must cancel t[0] modulo 2^64");
        let mut carry = cur >> 64;
        for j in 1..LIMBS {
            let cur = t[j] as u128 + (m as u128) * (n[j] as u128) + carry;
            t[j - 1] = cur as u64;
            carry = cur >> 64;
        }
        let cur = t[LIMBS] as u128 + carry;
        t[LIMBS - 1] = cur as u64;
        t[LIMBS] = t[LIMBS + 1] + (cur >> 64) as u64;
    }

    // CIOS guarantees the accumulator is below 2n. That is *not* the same as fitting in
    // 2048 bits: n may be nearly 2^2048, so 2n needs a 2049th bit. Reduce through the
    // wide accumulator rather than truncating.
    let mut acc: Ext = [0; LIMBS + 1];
    acc[..LIMBS].copy_from_slice(&t[..LIMBS]);
    acc[LIMBS] = t[LIMBS];
    ext_reduce(&mut acc, n);
    ext_narrow(&acc)
}

/// Convert `x` (in `[0, n)`) into Montgomery form: returns `x * R mod n`.
pub fn to_mont(x: &U2048, n: &U2048, n0inv: u64, r2: &U2048) -> U2048 {
    mont_mul(x, r2, n, n0inv)
}

/// Convert out of Montgomery form: returns `x * R^-1 mod n`.
pub fn from_mont(x: &U2048, n: &U2048, n0inv: u64) -> U2048 {
    mont_mul(x, &ONE, n, n0inv)
}

/// `base^exp mod n`, via square-and-multiply in Montgomery form.
///
/// `exp` is a value rather than a list of limbs so callers can pass `(n-1)/q` without
/// reshaping it.
pub fn pow_mod(base: &U2048, exp: &U2048, n: &U2048) -> U2048 {
    if is_zero(n) {
        panic!("pow_mod with zero modulus");
    }
    let n0inv = mont_n0inv(n);
    let r2 = mont_r2(n);
    let mut result = to_mont(&ONE, n, n0inv, &r2);
    let mut b = to_mont(&rem(base, n), n, n0inv, &r2);
    let bits = bit_len(exp);
    for i in 0..bits {
        if bit(exp, i) {
            result = mont_mul(&result, &b, n, n0inv);
        }
        b = mont_mul(&b, &b, n, n0inv);
    }
    from_mont(&result, n, n0inv)
}

/// Decode 32 little-endian bytes into a [`U2048`].
///
/// Byte order is little-endian, matching the `Read_U2048_LE(K_int) (mod N_pub)` of
/// FR-4.4.
pub fn from_le_bytes(bytes: &[u8; 256]) -> U2048 {
    let mut out = ZERO;
    for (i, limb) in out.iter_mut().enumerate() {
        let mut b = [0u8; 8];
        b.copy_from_slice(&bytes[i * 8..i * 8 + 8]);
        *limb = u64::from_le_bytes(b);
    }
    out
}

/// Encode a [`U2048`] as 32 little-endian bytes. Inverse of [`from_le_bytes`].
pub fn to_le_bytes(a: &U2048) -> [u8; 256] {
    let mut out = [0u8; 256];
    for (i, limb) in a.iter().enumerate() {
        out[i * 8..i * 8 + 8].copy_from_slice(&limb.to_le_bytes());
    }
    out
}

/// Parse a hex string, most significant nibble first, into a [`U2048`].
///
/// Accepts an optional `0x` prefix and any amount of whitespace.
pub fn from_hex(s: &str) -> Option<U2048> {
    let cleaned: String = s
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '_')
        .collect();
    let cleaned = cleaned
        .strip_prefix("0x")
        .or_else(|| cleaned.strip_prefix("0X"))
        .unwrap_or(&cleaned);
    if cleaned.is_empty() || cleaned.len() > 512 {
        return None;
    }
    // Pad on the left to a full 32 limbs -- 512 hex characters, since each 64-bit limb is
    // 16 hex characters. Padding only to a whole limb would place a short string in the
    // *top* limbs, silently turning a 1024-bit value into a 2048-bit one.
    let padded = if cleaned.len() == LIMBS * 16 {
        cleaned.to_string()
    } else {
        format!("{}{}", "0".repeat(LIMBS * 16 - cleaned.len()), cleaned)
    };
    let mut out = ZERO;
    for (i, chunk) in padded.as_bytes().chunks(16).enumerate() {
        let mut limb = [0u8; 8];
        for (j, byte) in limb.iter_mut().enumerate() {
            // `j` indexes *bytes*, `chunk` indexes *hex characters*, so the character
            // positions are 2*j and 2*j+1.
            let hi = nibble(chunk[j * 2])?;
            let lo = nibble(chunk[j * 2 + 1])?;
            *byte = (hi << 4) | lo;
        }
        // chunks are big-endian limbs; limb 0 is least significant.
        out[LIMBS - 1 - i] = u64::from_be_bytes(limb);
    }
    Some(out)
}

fn nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Render as lowercase hex, `0x`-prefixed, exactly 512 characters wide.
pub fn to_hex(a: &U2048) -> String {
    let mut s = String::with_capacity(2 + 512);
    s.push_str("0x");
    for i in (0..LIMBS).rev() {
        s.push_str(&format!("{:016x}", a[i]));
    }
    s
}

#[cfg(test)]
mod tests;
