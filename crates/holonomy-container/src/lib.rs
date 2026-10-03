//! The `.wavefunction` container: exactly 134,217,728 bytes of indistinguishable noise.
//!
//! Chaff `ChaCha20(K_chaff, nonce=i)` fills everything outside the payload, so the file
//! carries no magic number, no header and no cleartext block boundary. Access is
//! `O_DIRECT|O_SYNC`; opening seeks to Ω and reads only the 3-stage ring `[N−1, N, N+1]`
//! = 192 KiB, never the whole 128 MiB.
//!
//! Two salt rules, and the second one is easy to get wrong: the salt lives at **fixed
//! offset 0**, not at Ω. Ω is *derived from the passphrase*, so a salt stored at Ω would
//! be circular — you could not find the salt without the key you are trying to derive.
//!
//! Lands in Phase 3. Gate: create → close → open → read → write → close → open → verify,
//! plus a NIST SP 800-22 subset over the produced file. See PROJECT.md §5 Phase 3.

pub use holonomy_jail::PHASE_0_PLACEHOLDER;
