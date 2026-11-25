//! Block 10 Self-Hosting gate: authoring, formatting, bookmarking, compacting, verifying, and
//! exporting a real `.wavefunction` containing Holonomy's own v6 specification and release checks.

use std::fs;
use std::path::PathBuf;

use holonomy_eye::{FontSet, View};
use holonomy_gui::canvas::{apply_toolbar_action, PixelSurface, ToolbarAction};
use holonomy_gui::store::{self, Document};
use holonomy_gui::transfer::{
    export_to_path, import_from_path_with, TransferFormat, PLAINTEXT_EGRESS_DISCLOSURE,
};
use holonomy_gui::{App, Keymap};
use holonomy_seal::{generate_container_id, PassphraseInput};
use holonomy_shell::{Editor, Intent, QUIET_MD_THEME};
use holonomy_text::{ResidentSet, Typing};

struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn temp_file(label: &str, ext: &str) -> TempFile {
    let id = generate_container_id().expect("entropy");
    let hex: String = id.iter().map(|b| format!("{b:02x}")).collect();
    TempFile(std::env::temp_dir().join(format!("holonomy-selfhost-{label}-{hex}.{ext}")))
}

#[test]
fn self_hosting_spec_document_round_trips_through_format_bookmark_compact_and_export() {
    let pass = b"self-hosting-block10-passphrase";
    let doc_path = temp_file("spec", "wavefunction");
    let compacted_path = temp_file("spec-compact", "wavefunction");
    let exported_md = temp_file("spec-export", "md");
    let reimported_path = temp_file("spec-reimport", "wavefunction");

    let doc = Document::create_with(&doc_path.0, pass, store::test_params()).expect("create doc");
    let fonts = FontSet::embedded(QUIET_MD_THEME.body_size_px).expect("embedded fonts");
    let surface = PixelSurface::new(fonts.clone(), View::IDENTITY, 1_120, 760);
    let (lw, lh) = surface.logical_editor_extent();
    let residency = ResidentSet::new(doc.source(), store::BUDGET, store::SLACK).expect("residency");
    let mut editor = Editor::new(
        Typing::resume(doc.sink(), doc.spine().expect("spine"), store::CAPACITY),
        residency,
        fonts,
        lw,
        lh,
    );
    editor.set_view(View::IDENTITY);
    let mut app = App::new(editor, Keymap::new(lh), surface);

    let sections = [
        "Holonomy v6 Block 10 Specification\n",
        "Zero-trust encrypted prose with bounded 8 MiB residency.\n",
        "Every formatted word, heading, list item, and hyperlink lives on the Spine.\n",
        "Verified witness history supports 60-year scrubbing and one-way v5 migration.\n",
    ];
    let text: String = sections.concat();
    app.editor().apply(Intent::Type(text)).expect("type spec");

    // Apply headings, inline emphasis, code span, blockquote, bullet list, and hyperlink.
    let h1_end = "Holonomy v6 Block 10 Specification".len() as u64;
    app.editor().select_range(0, h1_end).expect("select H1");
    apply_toolbar_action(&mut app, ToolbarAction::Heading1).expect("H1");

    let bold_start = h1_end + 1;
    let bold_end = bold_start + "Zero-trust encrypted prose".len() as u64;
    app.editor()
        .select_range(bold_start, bold_end)
        .expect("select bold");
    apply_toolbar_action(&mut app, ToolbarAction::ToggleBold).expect("bold");

    let code_start = bold_end + " with ".len() as u64;
    let code_end = code_start + "bounded 8 MiB residency".len() as u64;
    app.editor()
        .select_range(code_start, code_end)
        .expect("select code");
    apply_toolbar_action(&mut app, ToolbarAction::ToggleCode).expect("code");

    let p3_start = (sections[0].len() + sections[1].len()) as u64;
    let p3_len =
        "Every formatted word, heading, list item, and hyperlink lives on the Spine.".len() as u64;
    let p3_end = p3_start + p3_len;
    app.editor()
        .select_range(p3_start, p3_end)
        .expect("select list/link");
    apply_toolbar_action(&mut app, ToolbarAction::ToggleBullet).expect("bullet");
    app.editor()
        .apply(Intent::SetLink(Some(
            "https://holonomy.invalid/block10".to_owned(),
        )))
        .expect("link");

    assert!(app.draw().expect("render self-hosted spec"));
    assert!(app.surface().painted_clusters() >= 80);

    app.editor().flush().expect("flush first sealed witness");
    let expected_styles = app.editor().typing().spine().styles().clone();
    assert!(!expected_styles.is_empty());
    drop(app);

    let seq = doc.witness_count().saturating_sub(1);
    doc.add_bookmark(seq, 1_763_942_400, "v6-selfhost-milestone")
        .expect("bookmark");
    let verify = doc.verify().expect("verify pre-compact");
    assert_eq!(verify.segments_verified, 1);

    let compacted = doc
        .compact_to(&compacted_path.0, pass)
        .expect("compact self-hosted container");
    drop(doc);
    let compact_verify = compacted.verify().expect("verify compacted container");
    assert_eq!(compact_verify.segments_verified, 1);
    assert_eq!(compacted.spine().expect("spine").styles(), &expected_styles);

    let report = export_to_path(
        &compacted,
        &exported_md.0,
        Some(TransferFormat::Markdown),
        true,
    )
    .expect("explicit markdown export");
    assert_eq!(report.disclosure, PLAINTEXT_EGRESS_DISCLOSURE);
    let md_body = fs::read_to_string(&exported_md.0).expect("read exported md");
    assert!(md_body.contains("# Holonomy v6 Block 10 Specification"));
    assert!(md_body.contains("**Zero-trust encrypted prose**"));
    assert!(md_body.contains("`bounded 8 MiB residency`"));
    let expected_bullet = "- [Every formatted word, heading, list item, and hyperlink lives on the Spine.](https://holonomy.invalid/block10)";
    assert!(md_body.contains(expected_bullet));

    let pass_input = PassphraseInput::new("self-hosting-block10-passphrase").expect("valid pass");
    let (reimported, reimport) = import_from_path_with(
        &exported_md.0,
        &reimported_path.0,
        &pass_input,
        store::test_params(),
        Some(TransferFormat::Markdown),
    )
    .expect("re-import exported spec");
    assert!(reimport.style_runs >= 4);
    assert!(reimported.verify().is_ok());
}
