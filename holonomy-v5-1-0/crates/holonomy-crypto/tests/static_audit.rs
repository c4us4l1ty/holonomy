//! Source-level guards against the invariants `v5.1.0` deleted.
//!
//! These are grep tests, and grep tests are usually a smell. They are justified here for one
//! reason: the properties they check are *absences*, and an absence cannot be asserted at runtime.
//! A future change that reintroduces `.text` hashing into the keystream would compile, would pass
//! every functional test (the container would still round-trip on the machine that built it), and
//! would silently make every container undecryptable on a different machine. This is the cheapest
//! place to catch that.
//!
//! Each test names the invariant it defends, so a failure points at the design decision rather
//! than at a string match.

use std::path::{Path, PathBuf};

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Every `.rs` file in this crate's `src/`, with its text.
fn all_sources() -> Vec<(PathBuf, String)> {
    fn walk(dir: &Path, out: &mut Vec<(PathBuf, String)>) {
        for entry in std::fs::read_dir(dir).expect("src/ is readable") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).expect("source is utf-8");
                out.push((path, text));
            }
        }
    }
    let mut out = Vec::new();
    walk(&src_dir(), &mut out);
    assert!(!out.is_empty(), "found no sources to audit");
    out
}

/// Strip line comments so a doc-comment *describing* a deleted mechanism does not trip the gate.
///
/// Only `//` comments are stripped, and deliberately not `/* */`: this crate does not use block
/// comments, and a naive block-comment stripper is a liability.
fn code_only(text: &str) -> String {
    text.lines()
        .map(|line| match line.find("//") {
            Some(i) => &line[..i],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn no_environmental_input_enters_a_key() {
    // Invariant: "AEAD key derivation must be 100% deterministic" (docs/PRD-v5.1.0.md §5.4).
    //
    // The forbidden identifiers are the ones the deleted `PEED` construction used. `std::time` is
    // included because a timestamp used as a nonce or salt is the same class of bug.
    const FORBIDDEN: &[&str] = &[
        "rdtsc",
        "RDTSC",
        "rdtscp",
        "caller_rip",
        "Caller_RIP",
        "PEED",
        "being_debugged",
        "BeingDebugged",
        "TracerPid",
        "tracer_pid",
        "std::time",
        "SystemTime",
        "Instant::now",
        "std::env",
        "std::process::id",
    ];

    let mut findings = Vec::new();
    for (path, text) in all_sources() {
        let code = code_only(&text);
        for needle in FORBIDDEN {
            if code.contains(needle) {
                findings.push(format!("{}: {needle}", path.display()));
            }
        }
    }
    assert!(
        findings.is_empty(),
        "environmental or timing input reached production code; keys must be deterministic:\n  {}",
        findings.join("\n  ")
    );
}

#[test]
fn no_branchless_decoy_selection() {
    // Invariant: one stratum per unlock. The deleted design kept `vault_handles[0]` (primary) and
    // `vault_handles[1]` (decoy) live simultaneously and selected between them arithmetically.
    // An indexed pair of root handles is the tell that this has come back.
    let mut findings = Vec::new();
    for (path, text) in all_sources() {
        let code = code_only(&text);
        for needle in ["vault_handles", "decoy_handle", "primary_handle"] {
            if code.contains(needle) {
                findings.push(format!("{}: {needle}", path.display()));
            }
        }
    }
    assert!(
        findings.is_empty(),
        "a second live root handle reappeared; only one stratum may be instantiated:\n  {}",
        findings.join("\n  ")
    );
}

#[test]
fn every_source_forbids_environment_dependence_in_its_docs() {
    // A weaker, positive check: the crate-level doc must state the determinism rule, so a reader
    // of any module sees why the absences above exist.
    let lib = std::fs::read_to_string(src_dir().join("lib.rs")).expect("lib.rs");
    assert!(lib.contains("Identical inputs produce identical keys"));
    assert!(lib.contains("deterministic"));
}

#[test]
fn unsafe_is_confined_to_the_lock_module() {
    // `#![forbid(unsafe_code)]` is not set on this crate because `lock.rs` needs mlock/madvise.
    // Everything else must be safe, and this test names the containment so it cannot broaden
    // without someone editing this list on purpose.
    let allowed = ["lock.rs", "rng.rs"];
    let mut findings = Vec::new();
    for (path, text) in all_sources() {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if path.ends_with("lib.rs") {
            continue;
        }
        if allowed.contains(&name.as_str()) {
            continue;
        }
        let code = code_only(&text);
        if code.contains("unsafe ") || code.contains("unsafe{") {
            findings.push(path.display().to_string());
        }
    }
    assert!(
        findings.is_empty(),
        "unsafe escaped the lock/rng modules:\n  {}",
        findings.join("\n  ")
    );
}
