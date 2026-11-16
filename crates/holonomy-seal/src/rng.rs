//! One place that draws OS randomness, so the choice is visible and mockable.
//!
//! Inherited from v5's `holonomy-crypto::rng`: every key and nonce draw funnels through here,
//! the v6 audit greps this file to prove nothing draws from anything weaker, and keygen paths
//! panic (never fall back) if the OS CSPRNG is unavailable — a key drawn from a substitute
//! source is worse than no key at all.
//!
//! The [`OsRng`] adapter implements `rand_core`'s 0.10 `TryRng` traits because `ml-kem` 0.3.4
//! depends on that version; a version split here would make the adapter implement a trait the
//! KEM does not ask for, so the pin is load-bearing.

use std::io;

/// Fill `buf` with OS entropy.
pub(crate) fn fill(buf: &mut [u8]) -> io::Result<()> {
    getrandom::getrandom(buf).map_err(|e| io::Error::other(format!("getrandom: {e}")))
}

/// 24 bytes of OS entropy (XChaCha nonces).
pub(crate) fn fill_24() -> [u8; 24] {
    let mut b = [0u8; 24];
    fill(&mut b).expect("OS CSPRNG unavailable");
    b
}

/// `rand_core` adapter over [`fill`], so the KEM's internal draws come from the same audited
/// place as ours. Implemented as `TryRng` + `TryCryptoRng` (the 0.10 traits); callers bridge
/// to `Rng` + `CryptoRng` via `rand_core::UnwrapErr`, which panics on error — matching the
/// no-fallback rule above.
#[derive(Debug, Clone, Copy)]
pub(crate) struct OsRng;

impl rand_core::TryRng for OsRng {
    type Error = std::io::Error;

    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        let mut b = [0u8; 4];
        fill(&mut b)?;
        Ok(u32::from_le_bytes(b))
    }

    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        let mut b = [0u8; 8];
        fill(&mut b)?;
        Ok(u64::from_le_bytes(b))
    }

    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Self::Error> {
        fill(dst)
    }
}

impl rand_core::TryCryptoRng for OsRng {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_draws_differ() {
        let a = fill_24();
        let b = fill_24();
        assert_ne!(a, b);
        assert_ne!(a, [0u8; 24]);
    }

    #[test]
    fn fills_the_whole_buffer() {
        let mut b = [0u8; 64];
        fill(&mut b).unwrap();
        assert!(b.iter().any(|&x| x != 0));
    }
}
