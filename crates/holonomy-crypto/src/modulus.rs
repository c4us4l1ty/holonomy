//! The VDF modulus and the sequential squaring chain that is the time-lock.
//!
//! # Why a composite with an unknown factorisation
//!
//! The chain is `S_i = S_{i-1}² (mod N_pub)`, run `T` times, and its soundness rests on
//! the exponent `2^T` being impossible to compress. That holds when `φ(N)` and `λ(N)` are
//! unknown, which means `N` must be composite with secret factors. A *prime* `N` would
//! publish `φ(N) = N − 1`, but that alone is not the break it looks like: obtaining
//! `2^T mod (N − 1)` is itself a chain of `T` squarings modulo `N − 1`, so the reduction
//! buys the attacker nothing. What is genuinely fatal is *publishing a factorisation of
//! `N − 1`*, because that hands over a complete map of the recursive CRT attack. PROJECT.md
//! §2.5 records this in full, including the argument that is easy to get wrong.
//!
//! # The constant
//!
//! [`N_PUB`] is the RSA Laboratories RSA-2048 challenge number: a 2048-bit semiprime whose
//! factors were generated and destroyed in 1991. Widely replicated, so its soundness does
//! not depend on our own key generation, and it is a compile-time constant in `.rodata` --
//! nothing is searched or proved at build time or in the test gate.
//!
//! # The `gcd(S_0, N) = 1` precondition
//!
//! If the seed shares a factor with `N`, it is a zero divisor in `Z/NZ`. The chain then
//! degenerates: one CRT component collapses toward zero and the other cycles in a much
//! smaller group, so the chain stops costing `T` squarings and the derived key is garbage.
//! By accident the odds are ~2^-1024, but a duress or wrong-passcode path must not be able
//! to reach it quietly, so [`sequential_squarings`] returns an error rather than a value.

use core::cmp::Ordering;

use crate::bignum::{
    from_hex_const, from_le_bytes, gcd, is_zero, mont_mul, mont_n0inv, mont_r2, rem, to_le_bytes,
    to_mont, U2048,
};

/// The RSA-2048 challenge number, 512 nibbles, most significant first.
const N_PUB_HEX: &str = "c7970ceedcc3b0754490201a7aa613cd73911081c790f5f1a8726f463550bb5b\
7ff0db8e1ea1189ec72f93d1650011bd721aeeacc2acde32a04107f0648c28\
13a31f5b0b7765ff8b44b4b6ffc93384b646eb09c7cf5e8592d40ea33c80039f\
35b4f14a04b51f7bfd781be4d1673164ba8eb991c2c4d730bbbe35f592bdef5\
24af7e8daefd26c66fc02c479af89d64d373f442709439de66ceb955f3ea37d5\
159f6135809f85334b5cb1813addc80cd05609f10ac6a95ad65872c909525bdad\
32bc729592642920f24c61dc5b3c3b7923e56b16a4d9d373d8721f24a3fc0f1b\
3131f55615172866bccc30f95054c824e733a5eb6817f7bc16399d48c6361cc7e5";

/// The VDF modulus: RSA-2048, a 2048-bit semiprime with unknown factors.
pub const N_PUB: U2048 = from_hex_const(N_PUB_HEX);

/// **The largest chain this build will run.** A ceiling on the VDF's iteration count.
///
/// # Why a ceiling at all
///
/// **Because `T` is written in the clear and read back before any key exists.** The container
/// keeps the iteration count in a plaintext page beside the salt, precisely so that a reader
/// knows what the derivation will cost before it pays for it — which means the file, and
/// therefore anyone who can write the file, chooses how long the unlock takes.
///
/// Without a ceiling, `T = u64::MAX` is not an error: it is 584 billion years of squarings.
/// The user is left holding a file that cannot be opened, the CPU is pinned at 100 % for as
/// long as they wait, and the only remedy is to delete the document. **A file must not be able
/// to hang the program that opens it.**
///
/// # Why this number, and not another
///
/// **The bound is a property of the hardware.** A 2048-bit Montgomery squaring is 32 limb
/// products plus a 32-limb reduction — at least 64 multiplications, chained through the
/// reduction, so no CPU on earth does it in less than roughly 100 ns. The unlock budget this
/// project targets is 250 ms, so the largest `T` any honest container can hold is
/// `250e6 / 100ns = 2.5e6`.
///
/// **This build measures 2,664 ns per squaring**, which is `T = 93,843`; a modern desktop CPU
/// running the same code is perhaps 3× faster, so `T ≈ 300,000`. A million is therefore
/// roughly 3× the fastest plausible honest container and a third of the unreachable
/// hardware bound: **headroom for any real host, and a hard stop for a hostile file.**
///
/// **And the cost of the ceiling being wrong is bounded.** A container written by a host
/// faster than 250 ns per squaring cannot be opened by this build — it fails with
/// [`VdfError::TooManyIterations`] rather than hanging, and the message says which number was
/// refused. The opposite error — a ceiling so low that a legitimate container is rejected —
/// is the one worth avoiding, and this number is chosen with that in mind.
pub const MAX_VDF_ITERATIONS: u64 = 1_000_000;

/// The committed literal, exposed so the gate can assert the parsed constant still
/// matches it rather than trusting the parser.
pub const fn n_pub_hex() -> &'static str {
    N_PUB_HEX
}

/// Montgomery constants for [`N_PUB`], derived once instead of per chain.
///
/// `mont_r2` is 4096 doubling steps, so recomputing it on every unlock would add real
/// latency to the one operation that is already the slowest thing in the program.
pub struct MontgomeryParams {
    /// `-N^-1 mod 2^64`.
    pub n0inv: u64,
    /// `R^2 mod N`, for converting into Montgomery form.
    pub r2: U2048,
}
/// Derive the Montgomery parameters for [`N_PUB`].
pub fn mont_params() -> MontgomeryParams {
    MontgomeryParams {
        n0inv: mont_n0inv(&N_PUB),
        r2: mont_r2(&N_PUB),
    }
}

/// Why a chain could not be run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VdfError {
    /// `gcd(seed, N_pub) != 1`: the seed is a zero divisor and the chain would degenerate.
    /// See the module docs.
    SeedNotCoprime,
    /// The seed reduced to zero, so the chain would be a no-op returning zero.
    ZeroSeed,
    /// `iterations` was zero, which is not a time-lock.
    ZeroIterations,
    /// The per-squaring cost used to derive the iteration count was zero, so the budget cannot be met.
    ///
    /// **Its own variant rather than a reused one.** An earlier version returned [`ZeroSeed`](Self::ZeroSeed)
    /// here, which is a different failure with a different fix — this one is an uncalibrated host, that one
    /// is a degenerate seed — and reusing a variant makes `Display` say "seed reduced to zero" about a
    /// measurement that never happened. A reader debugging a zero-iteration challenge would be sent to look
    /// at the seed.
    Uncalibrated,
    /// The chain was asked for more iterations than [`MAX_VDF_ITERATIONS`].
    ///
    /// **Its own variant, and it is the one that stops a denial of service.**
    ///
    /// The iteration count `T` is written into the container in the clear, beside the salt, and
    /// read back before the key exists — so a hostile file chooses the size of the computation
    /// this build is about to perform. `T = u64::MAX` is 584 billion years of squarings, and the
    /// unlock simply never returns: the user is holding a file that cannot be opened, and the
    /// only way out is to delete it.
    ///
    /// **The cap is a property of the hardware, not a policy.** A 2048-bit Montgomery squaring is
    /// at least 64 multiplications with a dependency chain through the reduction, so no CPU on
    /// earth does it in less than roughly 100 ns; the budget this project targets is 250 ms, so
    /// `T = 250e6 / 100ns = 2.5e6` is an unreachable upper bound for any honest container.
    /// [`MAX_VDF_ITERATIONS`] is a third of that, which leaves an order of magnitude of headroom
    /// over the fastest plausible host and still refuses a file that asks for a hang.
    TooManyIterations {
        /// What was asked for.
        requested: u64,
        /// The cap.
        cap: u64,
    },
}

impl core::fmt::Display for VdfError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::SeedNotCoprime => f.write_str("seed is not coprime with N_pub (would degenerate)"),
            Self::ZeroSeed => f.write_str("seed reduced to zero"),
            Self::ZeroIterations => f.write_str("zero iterations"),
            Self::Uncalibrated => f.write_str(
                "the per-squaring cost is zero, so an iteration count cannot be derived from it",
            ),
            Self::TooManyIterations { requested, cap } => write!(
                f,
                "{requested} iterations exceeds the {cap}-iteration ceiling for a VDF chain"
            ),
        }
    }
}

impl std::error::Error for VdfError {}

/// Reduce a 64-byte `K_int` into the seed for the chain: `S_0 = K_int mod N_pub`.
///
/// FR-4.4 specifies `S_0 = Read_U2048_LE(K_int) (mod N_pub)`, so the low 192 bytes of
/// `K_int` are zero-extended and the whole thing is taken little-endian.
pub fn seed_from_k_int(k_int: &[u8; 64]) -> U2048 {
    let mut wide = [0u8; 256];
    wide[..64].copy_from_slice(k_int);
    rem(&from_le_bytes(&wide), &N_PUB)
}

/// Run the sequential squaring chain `iterations` times: returns `S_0^(2^iterations) (mod N_pub)`.
///
/// Each step is one [`mont_mul`] of the accumulator with itself. In Montgomery form that
/// *is* the stored form of the square, so the loop has no per-step conversion and no
/// allocation -- which matters because this is the one loop that must not stop for
/// anything.
///
/// # The ceiling, and why it is here rather than in the caller
///
/// **`iterations` is attacker-reachable.** The container writes `T` into a cleartext page
/// beside the salt and reads it back before any key exists, so a file chooses the length of
/// this loop. Without a cap, a hostile file with `T = u64::MAX` does not fail -- it *hangs*,
/// for centuries, holding the CPU and the 128 MiB the Argon2 stage already allocated.
///
/// The check is here rather than at the call sites because this is the only function that
/// runs the loop: a cap enforced by a caller is a cap that a new caller can forget.
pub fn sequential_squarings(seed: &U2048, iterations: u64) -> Result<U2048, VdfError> {
    if iterations == 0 {
        return Err(VdfError::ZeroIterations);
    }
    // **Refused before anything is computed.** The zero check above is free because the chain
    // has not started; this one is equally free and stops the loop from starting at all.
    if iterations > MAX_VDF_ITERATIONS {
        return Err(VdfError::TooManyIterations {
            requested: iterations,
            cap: MAX_VDF_ITERATIONS,
        });
    }
    let s = rem(seed, &N_PUB);
    if is_zero(&s) {
        return Err(VdfError::ZeroSeed);
    }
    // Zero divisor check. gcd of a 2048-bit pair is microseconds, against a chain that is
    // seconds, so this is free.
    if gcd(s, N_PUB) != crate::bignum::ONE {
        return Err(VdfError::SeedNotCoprime);
    }

    let params = mont_params();
    let mut acc = to_mont(&s, &N_PUB, params.n0inv, &params.r2);
    for _ in 0..iterations {
        acc = mont_mul(&acc, &acc, &N_PUB, params.n0inv);
    }
    Ok(mont_mul(&acc, &crate::bignum::ONE, &N_PUB, params.n0inv))
}

/// The chain result as 256 little-endian bytes, for feeding `Blake2b-512(S_T || K_int)`.
pub fn chain_to_le_bytes(v: &U2048) -> [u8; 256] {
    to_le_bytes(v)
}

/// The modulus as 256 little-endian bytes, for interop with `Read_U2048_LE`.
pub fn n_pub_le_bytes() -> [u8; 256] {
    to_le_bytes(&N_PUB)
}

/// `N_pub` is strictly greater than `x`, used by callers sizing buffers against it.
pub fn exceeds(x: &U2048) -> bool {
    crate::bignum::cmp(x, &N_PUB) == Ordering::Less
}

#[cfg(test)]
mod tests;
