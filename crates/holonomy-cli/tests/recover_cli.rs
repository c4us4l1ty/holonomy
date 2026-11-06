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
    assert!(stderr.contains("does not prove the container is unrecoverable"), "{stderr}");
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
