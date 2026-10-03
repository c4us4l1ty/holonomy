//! The generated TypeScript contract is not stale.
//!
//! # Why this is an integration test
//!
//! It began as unit tests in `src/bridge.rs` and failed for a reason that is worth
//! more than the fix: `#[ts(export)]` makes `ts-rs` generate one `#[test]` per
//! type, all named `bridge::export_bindings_*`. A `cargo test` filter of
//! `bridge::` matches those as well, so cargo ran the *writers* and the staleness
//! check concurrently, in one process, against one file. The check read the file
//! part-way through a rewrite and reported that `BootPayload` was missing from a
//! file that contained `BootPayload` on disk.
//!
//! Two checks in the same module, one filter matching both. Moving here puts the
//! check in a different test binary — a different process, run after this one
//! finishes — where it cannot observe a half-written file.
//!
//! # What "not stale" means here
//!
//! Each Rust type's generated declaration is compared against the checked-in
//! `.ts` file, with whitespace and the `export` keyword normalised away. That is
//! the property that matters: if a field is renamed in Rust, `decl()` changes, the
//! checked-in file stops containing it, and this fails.
//!
//! It is deliberately *not* a byte-for-byte comparison of the whole file. `ts-rs`
//! owns the layout — the "do not edit" header, where `import` lines land, whether
//! comments are emitted — and pinning that would make a library upgrade look like a
//! contract change. A parity test between hand-written files is duplication in
//! disguise (DOCTRINE.md §8); a parity test between a generated file and the
//! generator that produced it is just a build step that nobody can forget. So the
//! comparison is on meaning, not formatting.
//!
//! Regenerate with `scripts/gen-bridge-types.sh`.

// The lib is named `holonomy_shell_lib`, not `holonomy_shell`: Tauri's convention
// is `<name>_lib`, so the app binary does not collide with it.
use holonomy_core::geometry::GeometryCalibration;
use holonomy_shell_lib::bridge::{
    BootPayload, HeightUpdate, LifecycleAction, LifecycleResult, ManifestSection, SectionContent,
};
use std::path::{Path, PathBuf};
use ts_rs::{Config, TS};

/// The repository root, from this crate's manifest directory.
///
/// `CARGO_MANIFEST_DIR` is `crates/holonomy-shell`, so the root is two levels up —
/// not three, which reaches `/home/von` and produces a path error that reads like a
/// missing file.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root is reachable")
}

/// Strip formatting so the comparison is about declarations, not layout.
///
/// Removes all whitespace and a leading `export`, so `export type Foo = { a: number }`
/// and `type Foo={a:number}` are equal.
fn normalise(ts: &str) -> String {
    ts.chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .trim_start_matches("export")
        .to_owned()
}

/// Assert the checked-in file contains what `ts-rs` would generate for `T`.
///
/// Reports the type and the path rather than a bare false, because the failure mode
/// is otherwise mysterious: someone renames a Rust field, and a string comparison
/// three files away says no.
fn assert_declared<T: TS + 'static>(file: &Path, expected_types: &[&str]) {
    let on_disk = std::fs::read_to_string(file)
        .unwrap_or_else(|e| panic!("{} could not be read: {e}", file.display()));
    let squashed = normalise(&on_disk);

    let decl = normalise(&T::decl(&Config::new()));
    let short = expected_types
        .iter()
        .find(|n| decl.starts_with(&format!("type{n}=")))
        .copied()
        .unwrap_or_else(|| {
            panic!(
                "{} does not begin with any of {expected_types:?}",
                file.display()
            )
        });

    assert!(
        squashed.contains(&decl),
        "{} is stale: it does not contain the declaration of `{short}` that the Rust\n\
         definition now generates.\n\n\
         Expected to find:\n  {decl}\n\n\
         Run scripts/gen-bridge-types.sh and commit the result.",
        file.display(),
    );
}

/// Every type that crosses the bridge, and which file owns it.
///
/// The split is not cosmetic: the two crates cannot write one file, because `ts-rs`
/// overwrites its target and two writers on one path is a race whose outcome
/// depends on which finished last. Disjoint paths make `cargo test` idempotent.
const BRIDGE_FILE: &str = "app/src/core/generated-bridge.ts";
const CALIBRATION_FILE: &str = "app/src/core/generated-calibration.ts";

#[test]
fn bridge_types_match_the_checked_in_file() {
    let root = repo_root();
    let file = root.join(BRIDGE_FILE);

    // Each call checks one type. The type's own name is passed alongside because
    // `decl()` is a generic-looking `type Foo = ...` and `manifestsection` is what
    // appears in the file after normalisation.
    assert_declared::<BootPayload>(&file, &["BootPayload"]);
    assert_declared::<ManifestSection>(&file, &["ManifestSection"]);
    assert_declared::<SectionContent>(&file, &["SectionContent"]);
    assert_declared::<HeightUpdate>(&file, &["HeightUpdate"]);
    assert_declared::<LifecycleAction>(&file, &["LifecycleAction"]);
    assert_declared::<LifecycleResult>(&file, &["LifecycleResult"]);
}

#[test]
fn calibration_matches_the_checked_in_file() {
    let file = repo_root().join(CALIBRATION_FILE);
    assert_declared::<GeometryCalibration>(&file, &["GeometryCalibration"]);
}

#[test]
fn the_frontend_imports_the_generated_types_rather_than_declaring_them() {
    // The generated file is worthless if the frontend hand-writes the same shapes
    // next to it, and nothing notices. `boot.ts` is the only place that used to do
    // that, so it is the one place worth checking.
    //
    // A type-only import is what a re-export looks like, and a redeclaration is
    // what a second copy looks like; both are `import type`. What distinguishes
    // them is what follows: `export type { ... } from` re-exports, `export
    // interface` declares. So the check is that no `export interface` and no
    // `export type X = {` appears in `boot.ts`.
    let boot = repo_root().join("app/src/core/boot.ts");
    let src = std::fs::read_to_string(&boot).expect("boot.ts is readable");

    for (what, pattern) in [
        ("an interface", r"export\s+interface\s+\w+"),
        ("an inline type alias", r"export\s+type\s+\w+\s*=\s*\{"),
    ] {
        assert!(
            !regex_lite(&src, pattern),
            "boot.ts declares {what}. The bridge contract is generated from Rust into\n\
             {BRIDGE_FILE}; a hand-written copy is the duplication ts-rs exists to remove\n\
             (DOCTRINE.md §8). Re-export the generated type instead."
        );
    }
}

/// A two-feature regex subset, because this is a test and not a regex engine.
///
/// Supports `\\s`, `\\w`, `+`, `*` and literal characters — which is all these two
/// patterns need, and adding a dependency to the build to do it properly is not a
/// trade worth making here.
fn regex_lite(haystack: &str, pattern: &str) -> bool {
    fn is_class(c: char) -> bool {
        c.is_whitespace() || c.is_alphanumeric() || c == '_'
    }
    let p: Vec<char> = pattern.chars().collect();
    let h: Vec<char> = haystack.chars().collect();
    let (mut pi, mut hi) = (0usize, 0usize);

    while hi <= h.len() {
        if pi == p.len() {
            return true;
        }
        // `\s` / `\w` followed by a quantifier.
        if p[pi] == '\\' && pi + 2 < p.len() {
            let (pred, quant): (fn(char) -> bool, char) = match p[pi + 1] {
                's' => (|c: char| c.is_whitespace(), p[pi + 2]),
                'w' => (|c: char| is_class(c), p[pi + 2]),
                _ => unreachable!("unsupported escape"),
            };
            let min = usize::from(quant != '*');
            let mut n = 0;
            while hi + n < h.len() && pred(h[hi + n]) {
                n += 1;
            }
            if n < min {
                return false;
            }
            pi += 3;
            hi += n;
            continue;
        }
        // `+` / `*` on a single character.
        if pi + 1 < p.len() && (p[pi + 1] == '+' || p[pi + 1] == '*') {
            let min = usize::from(p[pi + 1] == '+');
            let mut n = 0;
            while hi + n < h.len() && h[hi + n] == p[pi] {
                n += 1;
            }
            if n < min {
                return false;
            }
            pi += 2;
            hi += n;
            continue;
        }
        if hi == h.len() || h[hi] != p[pi] {
            pi = 0;
            hi += 1;
            continue;
        }
        pi += 1;
        hi += 1;
    }
    pi == p.len()
}
