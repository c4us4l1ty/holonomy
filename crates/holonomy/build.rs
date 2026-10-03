//! Bakes build-environment facts into the binary.
//!
//! Phase 0 records only the target triple, so the binary can report at runtime which
//! triple it was compiled for. That matters because a gnu build is a silent gate
//! failure: it runs fine on the build host and fails on the ThinkPad.
//!
//! Phase 2 extends this to run the `vdf-calibrate` bin once and bake
//! `NS_PER_SQUARING` in, so the VDF iteration count `T` becomes a compile-time
//! constant derived from a measurement rather than a guess (PROJECT.md §2.4).

fn main() {
    println!("cargo::rerun-if-changed=build.rs");

    // TARGET is provided to build scripts by Cargo; it is deliberately not visible to
    // `env!` inside the crate itself.
    let target = std::env::var("TARGET").expect("cargo always sets TARGET for build scripts");
    println!("cargo::rustc-env=HOLONOMY_TARGET={target}");
}
