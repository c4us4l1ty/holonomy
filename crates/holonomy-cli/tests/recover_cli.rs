//! `holonomy recover` finds the cursor an image needs, read-only, with synthetic credentials.
#![cfg(unix)]

use std::io::Write;
use std::process::{Command, Stdio};

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_holonomy")
}

fn credentials<'a>() -> [&'a str; 4] {
    ["--passphrase", "ci recovery passphrase", "--kappa", SHARE]
}

const SHARE: &str = "2222222222222222222222222222222222222222222222222222222222222222";

/// Provision, save once at cursor 1, then let `recover` work out the cursor without being told.
#[test]
fn recover_finds_the_cursor_of_a_container_without_writing_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("container");

    let provision = Command::new(binary())
        .arg("provision")
        .arg(&path)
        .args(credentials())
        .output()
        .expect("provision");
    assert!(
        provision.status.success(),
        "provision failed: {}",
        String::from_utf8_lossy(&provision.stderr)
    );

    let mut save = Command::new(binary())
        .arg("save")
        .arg(&path)
        .args(credentials())
        .args(["--cursor", "1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("save");
    save.stdin
        .take()
        .expect("stdin")
        .write_all(b"recover me")
        .expect("write document");
    let saved = save.wait_with_output().expect("save finished");
    assert!(
        saved.status.success(),
        "save failed: {}",
        String::from_utf8_lossy(&saved.stderr)
    );

    let before = std::fs::read(&path).expect("read container");
    let recovered = Command::new(binary())
        .arg("recover")
        .arg(&path)
        .args(credentials())
        .output()
        .expect("recover");
    assert!(
        recovered.status.success(),
        "recover failed: {}",
        String::from_utf8_lossy(&recovered.stderr)
    );
    let text = String::from_utf8_lossy(&recovered.stdout);
    assert!(text.contains("Cursor 1 authenticates"), "{text}");
    assert!(text.contains("Unkeyed image digest"), "{text}");
    assert!(text.contains("Stopped at the first cursor"), "{text}");
    assert!(text.contains("Nothing was written or ratcheted"), "{text}");
    assert_eq!(
        std::fs::read(&path).expect("read container again"),
        before,
        "recover must leave the container byte-identical"
    );
}

#[test]
fn recover_reports_a_failure_instead_of_a_cursor() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("container");
    let provision = Command::new(binary())
        .arg("provision")
        .arg(&path)
        .args(credentials())
        .output()
        .expect("provision");
    assert!(provision.status.success());

    let output = Command::new(binary())
        .arg("recover")
        .arg(&path)
        .args(["--passphrase", "the wrong passphrase"])
        .args(["--kappa", SHARE])
        .output()
        .expect("recover");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("No cursor from 0 to 53"), "{stderr}");
    assert!(stderr.contains("Last refusal"), "{stderr}");
    assert!(
        stderr.contains("does not prove the container is unrecoverable"),
        "{stderr}"
    );
}

#[test]
fn recover_refuses_an_explicit_cursor() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("container");
    std::fs::write(&path, b"not a container").expect("placeholder");
    let output = Command::new(binary())
        .arg("recover")
        .arg(&path)
        .args(credentials())
        .args(["--cursor", "3"])
        .output()
        .expect("recover");
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("use verify"));
}

/// Provision a container, save one document at `cursor`, and return its path and bytes.
fn saved_container(dir: &tempfile::TempDir, cursor: &str) -> (std::path::PathBuf, Vec<u8>) {
    let path = dir.path().join("container");
    let provision = Command::new(binary())
        .arg("provision")
        .arg(&path)
        .args(credentials())
        .output()
        .expect("provision");
    assert!(
        provision.status.success(),
        "provision failed: {}",
        String::from_utf8_lossy(&provision.stderr)
    );

    let mut save = Command::new(binary())
        .arg("save")
        .arg(&path)
        .args(credentials())
        .args(["--cursor", cursor])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("save");
    save.stdin
        .take()
        .expect("stdin")
        .write_all(b"a document for staged copies")
        .expect("write document");
    let saved = save.wait_with_output().expect("save finished");
    assert!(
        saved.status.success(),
        "save failed: {}",
        String::from_utf8_lossy(&saved.stderr)
    );
    let bytes = std::fs::read(&path).expect("read container");
    (path, bytes)
}

/// `--remnants` tests a staged copy with the same cursor scan, and still writes nothing.
#[test]
fn recover_tests_staged_copies_only_when_asked() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, before) = saved_container(&dir, "1");
    let staged = dir.path().join(".holonomy-save-crashed");
    std::fs::copy(&path, &staged).expect("stage a copy");
    let staged_before = std::fs::read(&staged).expect("read staged copy");

    let without = Command::new(binary())
        .arg("recover")
        .arg(&path)
        .args(credentials())
        .output()
        .expect("recover");
    assert!(without.status.success());
    let plain = String::from_utf8_lossy(&without.stdout);
    assert!(
        !plain.contains("Staged copy"),
        "the scan must be opt-in: {plain}"
    );
    assert!(
        !plain.contains(".holonomy-save-crashed"),
        "an unasked-for scan must not name leftovers: {plain}"
    );

    let output = Command::new(binary())
        .arg("recover")
        .arg(&path)
        .args(credentials())
        .arg("--remnants")
        .output()
        .expect("recover");
    assert!(
        output.status.success(),
        "recover failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("Staged copy"), "{text}");
    assert!(text.contains(".holonomy-save-crashed"), "{text}");
    assert_eq!(
        text.matches("Cursor 1 authenticates").count(),
        2,
        "the container and the staged copy both need cursor 1: {text}"
    );
    assert!(text.contains("candidate, not a choice"), "{text}");
    assert!(text.contains("Nothing was written or ratcheted"), "{text}");
    assert_eq!(
        std::fs::read(&path).expect("read container again"),
        before,
        "--remnants must leave the container byte-identical"
    );
    assert_eq!(
        std::fs::read(&staged).expect("read staged copy again"),
        staged_before,
        "--remnants must leave the staged copy byte-identical"
    );
}

/// A verified staged candidate can be copied to a new path without changing either source.
#[test]
fn recover_copy_preserves_sources_and_never_overwrites_the_destination() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, before) = saved_container(&dir, "1");
    let staged = dir.path().join(".holonomy-save-recovery-candidate");
    std::fs::copy(&path, &staged).expect("stage a copy");
    let staged_before = std::fs::read(&staged).expect("read staged copy");
    let recovered = dir.path().join("recovered-container");

    let output = Command::new(binary())
        .arg("recover-copy")
        .arg(&path)
        .args(["--remnant"])
        .arg(&staged)
        .args(["--cursor", "1", "--output"])
        .arg(&recovered)
        .args(credentials())
        .output()
        .expect("recover-copy");
    assert!(
        output.status.success(),
        "recover-copy failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("Verified staged image copied"), "{text}");
    assert!(
        text.contains("original container and staged image were not modified"),
        "{text}"
    );
    assert!(
        !text.contains("recover me"),
        "plaintext must not be printed: {text}"
    );
    assert_eq!(
        std::fs::read(&recovered).expect("read recovered copy"),
        before
    );
    assert_eq!(std::fs::read(&path).expect("read original"), before);
    assert_eq!(std::fs::read(&staged).expect("read remnant"), staged_before);

    let verify = Command::new(binary())
        .arg("verify")
        .arg(&recovered)
        .args(["--cursor", "1"])
        .args(credentials())
        .output()
        .expect("verify copied image");
    assert!(
        verify.status.success(),
        "the copy must remain independently openable: {}",
        String::from_utf8_lossy(&verify.stderr)
    );

    let original_destination = b"never overwrite me";
    std::fs::write(dir.path().join("already-exists"), original_destination)
        .expect("create existing destination");
    let refused = Command::new(binary())
        .arg("recover-copy")
        .arg(&path)
        .args(["--remnant"])
        .arg(&staged)
        .args(["--cursor", "1", "--output"])
        .arg(dir.path().join("already-exists"))
        .args(credentials())
        .output()
        .expect("recover-copy to existing destination");
    assert_eq!(refused.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&refused.stderr)
        .contains("refusing to overwrite existing destination"),);
    let existing =
        std::fs::read(dir.path().join("already-exists")).expect("read existing destination");
    assert_eq!(existing.as_slice(), original_destination);

    let unrelated = dir.path().join("ordinary-copy");
    std::fs::copy(&path, &unrelated).expect("create an unlisted copy");
    let refused = Command::new(binary())
        .arg("recover-copy")
        .arg(&path)
        .args(["--remnant"])
        .arg(&unrelated)
        .args(["--cursor", "1", "--output"])
        .arg(dir.path().join("must-not-exist"))
        .args(credentials())
        .output()
        .expect("recover-copy from an unlisted image");
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("run recover --remnants first")
    );
    assert!(!dir.path().join("must-not-exist").exists());
}

/// A leftover that is not a whole image is reported as cut short, not treated as an error.
#[test]
fn recover_reports_a_staged_copy_that_was_cut_short() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, before) = saved_container(&dir, "1");
    let staged = dir.path().join(".holonomy-save-cut-short");
    std::fs::write(&staged, vec![0x5au8; 4096]).expect("write a torn copy");

    let output = Command::new(binary())
        .arg("recover")
        .arg(&path)
        .args(credentials())
        .arg("--remnants")
        .output()
        .expect("recover");
    assert!(
        output.status.success(),
        "a torn leftover must not fail the scan: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains(".holonomy-save-cut-short"), "{text}");
    assert!(text.contains("Not a whole image"), "{text}");
    assert!(
        text.contains("Cursor 1 authenticates"),
        "the container still answers: {text}"
    );
    assert_eq!(
        std::fs::read(&path).expect("read container again"),
        before,
        "reporting a torn leftover must not write the container"
    );
    assert_eq!(
        std::fs::read(&staged).expect("read the leftover"),
        vec![0x5au8; 4096]
    );
}

/// A staged copy that opens while the container does not is a candidate, and the exit code says so.
#[test]
fn recover_reports_a_candidate_when_only_a_staged_copy_opens() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, before) = saved_container(&dir, "1");
    let staged = dir.path().join(".holonomy-save-crashed");
    std::fs::copy(&path, &staged).expect("stage a copy");
    std::fs::write(&path, vec![0u8; before.len()]).expect("damage the container");

    let output = Command::new(binary())
        .arg("recover")
        .arg(&path)
        .args(credentials())
        .arg("--remnants")
        .output()
        .expect("recover");
    assert_eq!(
        output.status.code(),
        Some(1),
        "the container's failure is still a failure"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stdout.contains("Staged copy"), "{stdout}");
    assert!(
        stdout.contains("Cursor 1 authenticates"),
        "the staged copy should still open: {stdout}"
    );
    assert!(
        stderr.contains("A staged copy opened while the container did not"),
        "{stderr}"
    );
    assert!(stderr.contains("not a repair"), "{stderr}");
    assert_eq!(
        std::fs::read(&staged).expect("read the staged copy"),
        before,
        "the candidate must be left exactly as it was"
    );
}
