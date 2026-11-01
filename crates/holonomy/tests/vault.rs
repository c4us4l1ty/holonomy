#![cfg(feature = "desktop")]
use holonomy::session::Session;
use holonomy::vault::{Vault, VaultError};
use holonomy_display::paint::Painter;
use holonomy_display::HeadlessScanout;
use holonomy_render::ChromeMetrics;
use holonomy_text::Editor;

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::current_exe().unwrap().ancestors().nth(3).unwrap().join("holonomy-vault-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(format!("{tag}-{}.wavefunction", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}
fn session() -> Session<'static> {
    static ATLAS: std::sync::OnceLock<&'static holonomy_assets::atlas::Atlas> = std::sync::OnceLock::new();
    let atlas = ATLAS.get_or_init(|| { let (a, _) = holonomy_assets::build_atlas(&[16]).unwrap(); Box::leak(Box::new(a)) });
    let m = ChromeMetrics::DESKTOP;
    Session::new(Editor::new(), Painter::new(atlas, 0), Box::new(HeadlessScanout::new(m.width, m.height)), m)
}
#[test]
fn two_passphrases_mount_isolated_multi_document_bundles_and_deletion_persists() {
    let path = scratch("isolation");
    Vault::create(&path, "top-secret", "top-secret", "normal", "normal", 8).unwrap();
    let mut s = session();
    {
        let mut v = Vault::open(&path, "top-secret", 8).unwrap();
        v.load_all(&mut s).unwrap();
        let id = v.next_id();
        s.create_document(id, "Private plan".into());
        s.insert_text("secret body").unwrap();
        s.editor_mut().style_range(0, 6, holonomy_text::STYLE_BOLD, 0).unwrap();
        v.save(&mut s).unwrap();
    }
    {
        let mut v = Vault::open(&path, "normal", 8).unwrap();
        v.load_all(&mut s).unwrap();
        assert_eq!(s.document_count(), 1);
        assert_eq!(s.state.title, "Untitled");
        assert!(!String::from_utf8(s.text().unwrap()).unwrap().contains("secret"));
        let id = v.next_id();
        s.create_document(id, "Shopping".into());
        s.insert_text("milk").unwrap();
        v.save(&mut s).unwrap();
        v.delete(&mut s, id).unwrap();
    }
    {
        let v = Vault::open(&path, "normal", 8).unwrap();
        assert_eq!(v.documents().len(), 1);
        assert_eq!(v.documents()[0].title, "Untitled");
    }
    {
        let mut v = Vault::open(&path, "top-secret", 8).unwrap();
        v.load_all(&mut s).unwrap();
        assert_eq!(s.document_count(), 2);
        s.switch_document_public(1);
        assert_eq!(s.state.title, "Private plan");
        assert_eq!(s.text().unwrap(), b"secret body");
        let payload = s.document_snapshots().unwrap().into_iter().find(|d| d.title == "Private plan").unwrap().payload;
        let restored = Editor::from_payload(&payload).unwrap();
        assert_eq!(restored.style_at(0).style_flags & holonomy_text::STYLE_BOLD, holonomy_text::STYLE_BOLD);
    }
    let _ = std::fs::remove_file(path);
}
#[test]
fn setup_refuses_overwrite_and_mismatched_confirmation() {
    let path = scratch("setup");
    assert!(matches!(Vault::create(&path, "a", "b", "c", "c", 8), Err(VaultError::ConfirmationMismatch)));
    assert!(!path.exists());
    Vault::create(&path, "a", "a", "c", "c", 8).unwrap();
    assert!(matches!(Vault::create(&path, "x", "x", "y", "y", 8), Err(VaultError::Exists)));
    let _ = std::fs::remove_file(path);
}
