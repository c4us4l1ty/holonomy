//! Block 10 integration tests: streaming `.txt`/`.md` import, explicit plaintext-egress export,
//! and one-way v5.1.0 -> v6 `.wavefunction` container migration.

use std::fs;
use std::io::{Cursor, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use holonomy_gui::store::{test_params, Document, CAPACITY};
use holonomy_gui::transfer::{
    export_to_path, export_to_writer, import_from_reader, migrate_v5_container_with, TransferError,
    TransferFormat, PLAINTEXT_EGRESS_DISCLOSURE,
};
use holonomy_seal::v5::{
    write_v5_test_container, V5PadSource, V5Passphrase, V5_PAYLOAD_PLAINTEXT_PER_SLOT,
};
use holonomy_seal::{ParamsClass, PassphraseInput};
use holonomy_text::{Heading, ListKind, SegmentSource};

fn temp_file(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "holonomy-xfer-{tag}-{}-{nanos}",
        std::process::id()
    ))
}

#[test]
fn streaming_txt_import_and_explicit_export_round_trip() {
    let dest = temp_file("txt.wf");
    let export_path = temp_file("out.txt");
    let pass = PassphraseInput::new("test-passphrase").unwrap();

    // > 100 KiB multi-byte UTF-8 input straddling exact 16 KiB segment boundaries.
    let unit = "αβγδεζηθ-holonomy-streaming-chunk-0123456789\n";
    let expected = unit.repeat(2_600);
    assert!(expected.len() > CAPACITY * 6);

    let (doc, rep) = import_from_reader(
        Cursor::new(expected.as_bytes()),
        &dest,
        &pass,
        test_params(),
        TransferFormat::PlainText,
    )
    .unwrap();
    assert_eq!(rep.format, TransferFormat::PlainText);
    assert_eq!(rep.plaintext_bytes as usize, expected.len());
    assert_eq!(rep.style_runs, 0);
    assert!(rep.segments_written >= 6);
    drop(doc);

    let reopened = Document::open_input(&dest, &pass).unwrap();
    let report = export_to_path(&reopened, &export_path, None, false).unwrap();
    assert_eq!(report.format, TransferFormat::PlainText);
    assert_eq!(report.bytes_written as usize, expected.len());
    assert_eq!(report.disclosure, PLAINTEXT_EGRESS_DISCLOSURE);
    assert_eq!(fs::read_to_string(&export_path).unwrap(), expected);

    // Refuses silently overwriting an existing plaintext file unless overwrite = true.
    assert!(matches!(
        export_to_path(&reopened, &export_path, None, false),
        Err(TransferError::AlreadyExists(_))
    ));
    let _ = fs::remove_file(&dest);
    let _ = fs::remove_file(&export_path);
}

#[test]
fn streaming_markdown_import_persists_styles_and_exports_cleanly() {
    let dest = temp_file("md.wf");
    let pass = PassphraseInput::new("md-passphrase").unwrap();
    let md = "\
# Main Title
> Quoted **emphasis** with `code`
- First bullet
2. Numbered *italic* ~~old~~ [doc](https://holonomy.example/spec)
---
Plain paragraph tail.
";

    let (doc, rep) = import_from_reader(
        Cursor::new(md.as_bytes()),
        &dest,
        &pass,
        test_params(),
        TransferFormat::Markdown,
    )
    .unwrap();
    assert_eq!(rep.format, TransferFormat::Markdown);
    assert!(rep.style_runs > 0);
    drop(doc);

    let reopened = Document::open_input(&dest, &pass).unwrap();
    let spine = reopened.spine().unwrap();
    assert!(spine
        .styles()
        .links()
        .contains(&"https://holonomy.example/spec".to_owned()));
    assert_eq!(spine.styles().style_at(0).heading, Heading::H1);

    let has_bullet = spine
        .styles()
        .runs()
        .iter()
        .any(|r| matches!(r.style.list, ListKind::Bullet { .. }));
    let has_num = spine
        .styles()
        .runs()
        .iter()
        .any(|r| matches!(r.style.list, ListKind::Ordered { start: 2, .. }));
    assert!(has_bullet);
    assert!(has_num);

    let mut md_out = Vec::new();
    let exp_rep =
        export_to_writer(&reopened, &mut md_out, "<mem>", TransferFormat::Markdown).unwrap();
    assert_eq!(exp_rep.disclosure, PLAINTEXT_EGRESS_DISCLOSURE);
    let roundtrip = String::from_utf8(md_out).unwrap();
    assert!(roundtrip.contains("# Main Title\n"));
    assert!(roundtrip.contains("- First bullet\n"));
    assert!(
        roundtrip.contains("2. Numbered *italic* ~~old~~ [doc](https://holonomy.example/spec)\n")
    );
    assert!(roundtrip.contains("---\n"));
    let _ = fs::remove_file(&dest);
}

#[test]
fn invalid_utf8_import_cleans_up_destination() {
    let dest = temp_file("bad-utf8.wf");
    let pass = PassphraseInput::new("bad-utf8").unwrap();
    let bad = b"hello \xff\xfe world";
    let res = import_from_reader(
        Cursor::new(&bad[..]),
        &dest,
        &pass,
        test_params(),
        TransferFormat::PlainText,
    );
    assert!(matches!(res, Err(TransferError::InvalidUtf8(_))));
    assert!(!dest.exists(), "failed import must remove partial file");
}

#[test]
fn migrate_v5_container_with_multibyte_slot_seam_and_corruption_cleanup() {
    let v5_path = temp_file("source.wf5");
    let v6_path = temp_file("migrated.wf");
    let bad_v6_path = temp_file("bad-migrated.wf");

    let kappa = [0x9Au8; 32];
    let v5_pass = V5Passphrase::new("Legacy-Mixed-Case").unwrap();
    let v6_pass = PassphraseInput::new("Fresh-V6-Passphrase").unwrap();
    let pad = V5PadSource::Derived { cursor: 5 };

    // Construct UTF-8 whose 4-byte emoji straddles the exact 65,488-byte v5 payload slot boundary!
    let mut payload = vec![b'a'; V5_PAYLOAD_PLAINTEXT_PER_SLOT - 2];
    payload.extend_from_slice("🦀".as_bytes());
    payload.extend_from_slice(b"-after-seam\n");
    let expected = String::from_utf8(payload.clone()).unwrap();

    write_v5_test_container(
        &v5_path,
        &v5_pass,
        &kappa,
        &pad,
        &payload,
        ParamsClass::Test,
    )
    .unwrap();
    let v5_before = fs::read(&v5_path).unwrap();

    let report = migrate_v5_container_with(
        &v5_path,
        &v6_path,
        &v5_pass,
        &v6_pass,
        &kappa,
        &pad,
        Some(5),
        ParamsClass::Test,
        test_params(),
    )
    .unwrap();
    assert_eq!(report.source_cursor, 5);
    assert_eq!(report.source_slots, 2);
    assert_eq!(report.imported_bytes as usize, expected.len());
    assert_eq!(report.imported_segments, report.verified_segments);

    // Source container is never mutated during one-way migration.
    assert_eq!(fs::read(&v5_path).unwrap(), v5_before);

    // Migrated v6 container opens under the fresh v6 key schedule with byte-exact content.
    let doc = Document::open_input(&v6_path, &v6_pass).unwrap();
    let spine = doc.spine().unwrap();
    let mut source = doc.source();
    let mut actual = Vec::new();
    for span in spine.spans_in(0, spine.len()) {
        let slab = span.slab();
        let plain = source.load(slab.segment()).unwrap();
        actual.extend_from_slice(&plain[slab.start() as usize..slab.end() as usize]);
    }
    assert_eq!(String::from_utf8(actual).unwrap(), expected);

    // Wrong passphrase cleans up destination container.
    let wrong_v5 = V5Passphrase::new("wrong").unwrap();
    let fail = migrate_v5_container_with(
        &v5_path,
        &bad_v6_path,
        &wrong_v5,
        &v6_pass,
        &kappa,
        &pad,
        Some(5),
        ParamsClass::Test,
        test_params(),
    );
    assert!(fail.is_err());
    assert!(!bad_v6_path.exists());

    // Non-UTF-8 legacy bytes inside v5 container fail cleanly and never produce a broken v6 doc.
    let v5_non_utf8 = temp_file("nonutf8.wf5");
    write_v5_test_container(
        &v5_non_utf8,
        &v5_pass,
        &kappa,
        &V5PadSource::None,
        &[0xff, 0xfe, 0x00],
        ParamsClass::Test,
    )
    .unwrap();
    let non_utf8_res = migrate_v5_container_with(
        &v5_non_utf8,
        &bad_v6_path,
        &v5_pass,
        &v6_pass,
        &kappa,
        &V5PadSource::None,
        Some(0),
        ParamsClass::Test,
        test_params(),
    );
    assert!(non_utf8_res.is_err());
    assert!(!bad_v6_path.exists());

    let _ = fs::remove_file(&v5_path);
    let _ = fs::remove_file(&v5_non_utf8);
    let _ = fs::remove_file(&v6_path);
}

fn run_holonomy_with_stdin(args: &[&str], stdin_data: &str) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_holonomy"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawns holonomy binary");
    if !stdin_data.is_empty() {
        child
            .stdin
            .as_mut()
            .expect("piped stdin")
            .write_all(stdin_data.as_bytes())
            .expect("writes passphrase");
    }
    child.wait_with_output().expect("waits for holonomy")
}

#[test]
fn cli_import_verify_and_export_subcommands_round_trip() {
    let src_md = temp_file("cli-src.md");
    let wf_path = temp_file("cli-doc.wf");
    let out_md = temp_file("cli-out.md");
    let md_body = "# CLI Heading\n\nBody with **bold** and `mono`.\n";
    fs::write(&src_md, md_body).unwrap();

    let help = run_holonomy_with_stdin(&["--help"], "");
    assert!(help.status.success());
    let help_text = String::from_utf8_lossy(&help.stdout);
    assert!(help_text.contains("import"));
    assert!(help_text.contains("export"));
    assert!(help_text.contains("migrate-v5"));
    assert!(help_text.contains("gui"));

    let pass_line = "cli-roundtrip-passphrase\n";
    let import_out = run_holonomy_with_stdin(
        &[
            "import",
            "--format",
            "md",
            src_md.to_str().unwrap(),
            wf_path.to_str().unwrap(),
        ],
        pass_line,
    );
    assert!(
        import_out.status.success(),
        "import stderr: {}",
        String::from_utf8_lossy(&import_out.stderr)
    );
    assert!(wf_path.exists());

    let verify_out = run_holonomy_with_stdin(&["verify", wf_path.to_str().unwrap()], pass_line);
    assert!(verify_out.status.success());
    let verify_stdout = String::from_utf8_lossy(&verify_out.stdout);
    assert!(verify_stdout.contains("verified"));

    let export_out = run_holonomy_with_stdin(
        &[
            "export",
            "--format",
            "md",
            wf_path.to_str().unwrap(),
            out_md.to_str().unwrap(),
        ],
        pass_line,
    );
    assert!(
        export_out.status.success(),
        "export stderr: {}",
        String::from_utf8_lossy(&export_out.stderr)
    );
    let export_stdout = String::from_utf8_lossy(&export_out.stdout);
    assert!(export_stdout.contains(PLAINTEXT_EGRESS_DISCLOSURE));
    assert_eq!(fs::read_to_string(&out_md).unwrap(), md_body);

    // Refuses overwrite unless `--force` is given.
    let refuse = run_holonomy_with_stdin(
        &[
            "export",
            "--format",
            "md",
            wf_path.to_str().unwrap(),
            out_md.to_str().unwrap(),
        ],
        pass_line,
    );
    assert!(!refuse.status.success());

    let forced = run_holonomy_with_stdin(
        &[
            "export",
            "--force",
            "--format",
            "md",
            wf_path.to_str().unwrap(),
            out_md.to_str().unwrap(),
        ],
        pass_line,
    );
    assert!(forced.status.success());

    let _ = fs::remove_file(&src_md);
    let _ = fs::remove_file(&wf_path);
    let _ = fs::remove_file(&out_md);
}
