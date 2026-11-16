//! The Witness — the append-only `.wavefunction` world-line container (ROADMAP Block 3).
//!
//! Block 3 has not begun; its construction is specified in
//! `docs/rfc/RFC-002-the-witness.md`. What exists now is the crate's constitutional clause:
//!
//! > **History is storage, not a feature.** The file is an append-only log of independently
//! > sealed segments with an authenticated witness chain. There is no bulk-decrypt operation,
//! > no overwrite, and no fixed-size ceiling — the v5 134 MiB cap dies here. Power loss lands
//! > on the last committed witness, by construction (L-6), and cloud sync moves only appended
//! > tail bytes (L-10).

/// The append-only law, carried in code so it is compiled, not just quoted.
pub const APPEND_ONLY_LAW: &str =
    "writes append sealed segments and commit a witness; nothing overwrites, nothing precedes the chain";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_law_is_written() {
        assert!(!APPEND_ONLY_LAW.is_empty());
    }
}
