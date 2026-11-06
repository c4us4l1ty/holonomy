//! `holonomy remnants` reports crash leftovers without credentials and without decrypting.
#![cfg(unix)]

use std::process::Command;

fn run(args: &[&std::ffi::OsStr]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_holonomy"))
        .args(args)
        .output()
        .expect("run the binary")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn remnants_reports_staged_images_without_asking_for_credentials() {
    use blake2::{Blake2b512, Digest};

    let dir = tempfile::tempdir().expect("tempdir");
    let container = dir.path().join("book");
    std::fs::write(&container, vec![0x5Au8; 4096]).expect("container");
    let staged = dir.path().join(".holonomy-save-crashed");
    let payload = b"staged encrypted image";
    std::fs::write(&staged, payload).expect("staged image");

    let output = run(&[std::ffi::OsStr::new("remnants"), container.as_os_str()]);
    assert!(
        output.status.success(),
        "remnants failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains(".holonomy-save-crashed"), "{text}");
    assert!(text.contains(&format!("{} bytes", payload.len())), "{text}");
    assert!(text.contains("not a full container image"), "{text}");
    assert!(text.contains("holonomy verify"), "{text}");
    assert!(text.contains("delete one only after"), "{text}");

    let expected: [u8; 32] = {
        let digest = Blake2b512::digest(payload);
        let mut out = [0u8; 32];
        out.copy_from_slice(&digest[..32]);
        out
    };
    assert!(text.contains(&hex(&expected)), "{text}");
    // No prompt, no prompt text, no credential material on either stream.
    assert!(output.stderr.is_empty(), "{text}");
    assert!(!text.contains("kappa"), "{text}");
    assert!(!text.contains("assphrase"), "{text}");
}

#[test]
fn remnants_reports_an_empty_directory_plainly() {
    let dir = tempfile::tempdir().expect("tempdir");
    let container = dir.path().join("book");
    std::fs::write(&container, vec![0u8; 32]).expect("container");
    let output = run(&[std::ffi::OsStr::new("remnants"), container.as_os_str()]);
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("No interrupted-save remnants"), "{text}");
}

#[test]
fn remnants_rejects_flags_and_extra_paths() {
    let dir = tempfile::tempdir().expect("tempdir");
    let container = dir.path().join("book");
    std::fs::write(&container, vec![0u8; 32]).expect("container");

    for args in [
        vec![std::ffi::OsStr::new("remnants")],
        vec![
            std::ffi::OsStr::new("remnants"),
            container.as_os_str(),
            container.as_os_str(),
        ],
        vec![
            std::ffi::OsStr::new("remnants"),
            std::ffi::OsStr::new("--passphrase"),
            std::ffi::OsStr::new("nope"),
        ],
    ] {
        let output = run(&args);
        assert_eq!(output.status.code(), Some(2), "args: {args:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("remnants"),
            "{args:?}"
        );
    }
}
