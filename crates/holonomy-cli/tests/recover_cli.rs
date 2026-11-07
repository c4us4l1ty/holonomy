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
        !plain.contains("Candidate file"),
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
    assert!(text.contains("Candidate file"), "{text}");
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
    use std::os::unix::fs::MetadataExt;

    let dir = tempfile::tempdir().expect("tempdir");
    let (path, before) = saved_container(&dir, "1");
    let staged = dir.path().join(".holonomy-save-recovery-candidate");
    std::fs::copy(&path, &staged).expect("stage a copy");
    let staged_before = std::fs::read(&staged).expect("read staged copy");
    // More than the 20-entry diagnostic cap, with lexically earlier names: explicit copy must
    // still validate this selected candidate without requiring it to appear in the report.
    for index in 0..21 {
        std::fs::write(
            dir.path().join(format!(".holonomy-save-{index:03}")),
            b"short",
        )
        .expect("add earlier diagnostic entries");
    }
    let listing = Command::new(binary())
        .arg("recover")
        .arg(&path)
        .args(credentials())
        .arg("--remnants")
        .output()
        .expect("bounded remnant listing");
    assert!(listing.status.success());
    let listing_text = String::from_utf8_lossy(&listing.stdout);
    let truncation_note = "Only the first 20 matching files by filename were tested; more exist.";
    assert!(listing_text.contains(truncation_note), "{listing_text}");
    assert!(
        !listing_text.contains(".holonomy-save-recovery-candidate"),
        "the 21st lexical remnant should be beyond the display cap: {listing_text}"
    );

    let container_metadata = std::fs::metadata(&path).expect("container metadata");
    let mismatched_origin = dir.path().join(format!(
        ".holonomy-save-v2-{:016x}-{:016x}-{}-testnonce",
        container_metadata.dev(),
        container_metadata.ino(),
        "0".repeat(64)
    ));
    std::fs::hard_link(&path, &mismatched_origin)
        .expect("link a deliberately mismatched candidate");
    let mismatched_destination = dir.path().join("mismatched-origin-must-not-copy");
    let mismatched = Command::new(binary())
        .arg("recover-copy")
        .arg(&path)
        .args(["--remnant"])
        .arg(&mismatched_origin)
        .args(["--cursor", "1", "--output"])
        .arg(&mismatched_destination)
        .output()
        .expect("recover-copy with mismatched origin marker");
    assert_eq!(mismatched.status.code(), Some(1));
    let mismatched_stderr = String::from_utf8_lossy(&mismatched.stderr);
    assert!(
        mismatched_stderr.contains("origin does not match this container"),
        "{mismatched_stderr}"
    );
    assert!(!mismatched_destination.exists());

    let unbound_destination = dir.path().join("unbound-must-be-opted-in");
    let unbound_refused = Command::new(binary())
        .arg("recover-copy")
        .arg(&path)
        .args(["--remnant"])
        .arg(&staged)
        .args(["--cursor", "1", "--output"])
        .arg(&unbound_destination)
        .output()
        .expect("recover-copy without legacy opt-in");
    assert_eq!(unbound_refused.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&unbound_refused.stderr)
        .contains("legacy remnant is unbound to this container"));
    assert!(!unbound_destination.exists());

    let recovered = dir.path().join("recovered-container");

    let output = Command::new(binary())
        .arg("recover-copy")
        .arg(&path)
        .args(["--remnant"])
        .arg(&staged)
        .args(["--cursor", "1", "--allow-unbound", "--output"])
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
    assert!(
        text.contains("Verified staged image copied and read back at"),
        "{text}"
    );
    assert!(
        text.contains("Container and remnant contents were not written"),
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
    let refused_stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(refused_stderr.contains("selected path is not a regular .holonomy-save-* sibling"));
    assert!(!dir.path().join("must-not-exist").exists());
}

/// An incomplete leftover is reported without guessing why it is incomplete or failing the scan.
#[test]
fn recover_reports_an_incomplete_staged_copy_without_failing_the_scan() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, before) = saved_container(&dir, "1");
    let staged = dir.path().join(".holonomy-save-incomplete");
    std::fs::write(&staged, vec![0x5au8; 4096]).expect("write a short remnant");

    let output = Command::new(binary())
        .arg("recover")
        .arg(&path)
        .args(credentials())
        .arg("--remnants")
        .output()
        .expect("recover");
    assert!(
        output.status.success(),
        "an incomplete leftover must not fail the scan: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains(".holonomy-save-incomplete"), "{text}");
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
    assert!(stdout.contains("Candidate file"), "{stdout}");
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
