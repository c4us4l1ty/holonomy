//! End-to-end candidate inspection uses synthetic credentials and demo pad only.
#![cfg(unix)]

use std::io::{Read, Write};
use std::process::{Command, Stdio};

use blake2::{Blake2b512, Digest};

fn digest(path: &std::path::Path) -> Vec<u8> {
    let mut input = std::fs::File::open(path).expect("open image");
    let mut hash = Blake2b512::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let count = input.read(&mut buf).expect("read image");
        if count == 0 {
            break;
        }
        hash.update(&buf[..count]);
    }
    hash.finalize().to_vec()
}

#[test]
fn verify_checks_candidate_without_printing_or_modifying_plaintext() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("candidate");
    let binary = env!("CARGO_BIN_EXE_holonomy");
    let root = "11".repeat(32);
    let secret = "synthetic verification test document";
    let credentials = ["--passphrase", "ci test passphrase", "--kappa", &root];

    let create = Command::new(binary)
        .arg("provision")
        .arg(&path)
        .args(credentials)
        .output()
        .expect("provision command");
    assert!(
        create.status.success(),
        "provision failed: {}",
        String::from_utf8_lossy(&create.stderr)
    );
    let mut save = Command::new(binary)
        .arg("save")
        .arg(&path)
        .args(credentials)
        .args(["--cursor", "1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("save command");
    save.stdin
        .take()
        .expect("stdin")
        .write_all(secret.as_bytes())
        .expect("write input");
    let saved = save.wait_with_output().expect("save finished");
    assert!(
        saved.status.success(),
        "save failed: {}",
        String::from_utf8_lossy(&saved.stderr)
    );
    let before = digest(&path);
    let verify = Command::new(binary)
        .arg("verify")
        .arg(&path)
        .args(credentials)
        .args(["--cursor", "1"])
        .output()
        .expect("verify command");
    assert!(
        verify.status.success(),
        "verify failed: {}",
        String::from_utf8_lossy(&verify.stderr)
    );
    let stdout = String::from_utf8(verify.stdout).expect("UTF-8 report");
    let save_stdout = String::from_utf8(saved.stdout).expect("UTF-8 save report");
    let saved_digest = save_stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix("image digest "))
        .expect("save digest");
    assert_eq!(saved_digest.len(), 64);
    assert!(stdout.contains(&format!("image digest {saved_digest} ")));
    assert!(stdout.contains("not a durability or authenticity proof"));
    assert!(stdout.contains("Candidate authenticated at cursor 1"));
    assert!(stdout.contains("unused image bytes are not checked"));
    assert!(stdout.contains("does not verify document format"));
    assert!(!stdout.contains(secret));
    assert!(!String::from_utf8_lossy(&verify.stderr).contains(secret));
    assert_eq!(
        digest(&path),
        before,
        "verification must not modify container bytes"
    );

    let missing = Command::new(binary)
        .arg("verify")
        .arg(&path)
        .output()
        .expect("missing cursor");
    assert_eq!(missing.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("explicit --cursor"));
    let wrong = Command::new(binary)
        .arg("verify")
        .arg(&path)
        .args(credentials)
        .args(["--cursor", "0"])
        .output()
        .expect("wrong cursor");
    assert!(!wrong.status.success());
    assert!(!String::from_utf8_lossy(&wrong.stdout).contains(secret));
    assert_eq!(digest(&path), before);
}
