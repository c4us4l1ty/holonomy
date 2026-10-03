//! Holonomy — bare-silicon word processor, encrypted container and sealed OS jail.
//!
//! Phase 0: this binary does not yet open a container, a DRM buffer or a jail. It
//! exists to prove the build skeleton, which is the only thing Phase 0 asks for:
//! a static stripped musl binary that `ldd` calls `statically linked`, and a
//! `hardware` feature flag that gates hardware-only paths.
//!
//! Print the boot banner and exit. The real boot sequence lands in Phase 8, and it
//! runs in a fixed order that is easy to get wrong from memory — see the crate
//! comment in `Cargo.toml`.

/// Release baseline. Plan.md Part 4 calls this v3.0.0-SINGULARITY; Cargo wants a
/// lowercase semver pre-release, so the codename is normalised to `singularity`.
const RELEASE_BASELINE: &str = env!("CARGO_PKG_VERSION");

/// The target triple this binary was actually compiled for.
///
/// Baked in by `build.rs` at compile time, so this cannot drift from the build. It is
/// printed because a gnu binary is a gate failure (NFR-2.3) that is otherwise invisible
/// until someone tries to run it on the ThinkPad.
const TARGET_TRIPLE: &str = env!("HOLONOMY_TARGET");

fn main() {
    println!("holonomy {RELEASE_BASELINE}");
    println!("target:  {TARGET_TRIPLE}");

    if cfg!(feature = "hardware") {
        println!("backends: DrmScanout, EvdevSource (hardware compiled in)");
    } else {
        println!("backends: HeadlessScanout, ScriptedInputSource (hardware gated off)");
    }

    // The Zero-Compositor Invariant (Plan.md Part 4 §1) is structural, so it is worth
    // stating at runtime rather than only in a doc comment: a build that reached this
    // point did not link a display server.
    println!("compositor: none");

    // The derivation parameters are printed rather than hard-coded silently: if these
    // ever differ from PROJECT.md §2.4 the unlock budget is wrong, and the place that
    // finds out should not be a user's failed unlock.
    println!(
        "kdf:      argon2id m={} KiB t={} p={}",
        KDF_M_COST_KIB, KDF_T_COST, KDF_P_COST
    );
    println!(
        "modulus:  RSA-2048 ({} bits), composite, factors unknown",
        MODULUS_BITS
    );
    println!("vdf:      T is derived from a measured per-squaring cost, not fixed");

    // Phase 0 gate is the skeleton; the container, display and jail land in Phases 3-8.
    println!("phase: 0 -- build skeleton; no container, display or jail yet");
}

/// Argon2id memory cost in KiB, from PROJECT.md §2.4.
const KDF_M_COST_KIB: u32 = holonomy_crypto::envelope::ARGON2_M_COST_KIB;
/// Argon2id time cost, from PROJECT.md §2.4.
const KDF_T_COST: u32 = holonomy_crypto::envelope::ARGON2_T_COST;
/// Argon2id parallelism, from PROJECT.md §2.4.
const KDF_P_COST: u32 = holonomy_crypto::envelope::ARGON2_P_COST;

/// Bit length of the VDF modulus, read from the constant itself rather than written as a
/// literal. If `N_PUB` were ever replaced with a differently-sized modulus, this line would
/// report the new size instead of continuing to claim 2048.
const MODULUS_BITS: usize = bit_length(&holonomy_crypto::modulus::N_PUB);

/// Bit length of a fixed-width little-endian-limb integer.
///
/// `const fn` so `MODULUS_BITS` is computed during compilation. A plain `fn` cannot be
/// called in a `const` initialiser.
const fn bit_length(v: &[u64]) -> usize {
    let mut i = v.len();
    while i > 0 {
        i -= 1;
        let limb = v[i];
        if limb != 0 {
            return i * 64 + (64 - limb.leading_zeros() as usize);
        }
    }
    0
}
