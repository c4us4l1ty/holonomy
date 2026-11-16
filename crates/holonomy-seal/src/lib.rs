//! The Seal — the blessed cryptographic boundary (ROADMAP Block 2).
//!
//! Block 2 has not begun; its construction is specified in `docs/rfc/RFC-001-the-seal.md`.
//! What exists now is the crate's constitutional clause, so the rule is compiled into every
//! build from day one:
//!
//! > **One boundary.** All primitives — Argon2id, ML-KEM-1024, X25519, CMT-4 AEAD — enter the
//! > system exclusively through this crate, behind capability tokens the compiler enforces.
//! > Code that does not hold the token type cannot receive key bytes. The v5.1.0 envelope shape
//! > (two-phase, non-circular, audit-proven) is inherited; the mandatory USB pad is not.

/// The boundary law, carried in code so it is compiled, not just quoted.
pub const BOUNDARY_LAW: &str = "key material crosses one boundary, held only behind capability tokens";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_law_is_written() {
        assert!(!BOUNDARY_LAW.is_empty());
    }
}
