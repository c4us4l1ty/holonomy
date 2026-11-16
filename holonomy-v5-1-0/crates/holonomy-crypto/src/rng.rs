//! One place that draws OS randomness, so it can be mocked and so the CSPRNG choice is visible.
//!
//! `getrandom` is a single-symbol dependency whose whole job is `getrandom(2)` /
//! `BCryptGenRandom` / `SecRandomCopyBytes`. Wrapping it keeps a would-be deterministic test from
//! having to fight a global RNG, and gives the static audit one file to look at when it checks
//! that no key material is drawn from anything weaker.

use std::io;

/// Fill `buf` with OS entropy.
pub fn fill(buf: &mut [u8]) -> io::Result<()> {
    getrandom::getrandom(buf).map_err(|e| io::Error::other(format!("getrandom: {e}")))
}

/// 32 bytes of OS entropy, for keys and nonces that must not repeat.
pub fn fill_32() -> [u8; 32] {
    let mut b = [0u8; 32];
    fill(&mut b).expect("OS CSPRNG unavailable");
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_draws_differ() {
        let a = fill_32();
        let b = fill_32();
        assert_ne!(a, b);
        assert_ne!(a, [0u8; 32]);
    }

    #[test]
    fn fills_the_whole_buffer() {
        let mut b = [0u8; 64];
        fill(&mut b).unwrap();
        assert!(b.iter().any(|&x| x != 0));
    }
}
