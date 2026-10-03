//! Build script for the Tauri shell.
//!
//! Minimal by design. There is no `tauri.conf.json` codegen step needed here
//! beyond what `tauri-build` does itself, and no build-time asset generation:
//! the frontend is served by Vite in development and pre-built by
//! `beforeBuildCommand` in release.

fn main() {
    tauri_build::build()
}