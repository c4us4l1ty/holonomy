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

    // Phase 0 only. Every phase below this line is not implemented yet, and saying so
    // is more useful than pretending otherwise.
    println!("phase: 0 -- build skeleton; no container, display or jail yet");
}
