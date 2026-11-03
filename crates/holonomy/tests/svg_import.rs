#![cfg(feature = "desktop")]
//! A user-selected SVG becomes an ordinary PNG image before persistence, paint or export.
use holonomy::svg_import;
use holonomy::Session;
use holonomy_display::paint::Painter;
use holonomy_display::HeadlessScanout;
use holonomy_export::{self, Format};
use holonomy_render::chrome::ChromeMetrics;
use holonomy_text::Editor;

#[test]
fn svg_persists_and_exports_as_png_without_keeping_any_xml() {
    let source = br##"<svg xmlns="http://www.w3.org/2000/svg" width="80" height="40"><circle cx="40" cy="20" r="16" fill="#1a73e8"/></svg>"##;
    let png = svg_import::rasterize_svg(source).expect("static SVG rasterises");
    let atlas = Box::leak(Box::new(holonomy_assets::build_atlas(&[16]).unwrap().0));
    let m = ChromeMetrics::DESKTOP;
    let mut session = Session::new(
        Editor::new(),
        Painter::new(atlas, 0),
        Box::new(HeadlessScanout::new(m.width, m.height)),
        m,
    );
    session.set_managed_documents(true);
    session.insert_image_bytes(&png).expect("insert as PNG");
    session.paint(None).expect("paint same PNG decoder path");
    assert!(session.image_cache_pixels(0).is_some(), "image is resident");

    let payload = session.editor().payload().expect("serialize");
    assert!(!payload.windows(4).any(|w| w == b"<svg"), "source markup was never persisted");
    let restored = Editor::from_payload(&payload).expect("deserialize");
    assert_eq!(restored.assets().len(), 1);
    assert_eq!(restored.assets().get(0).unwrap().png.as_slice(), png.as_slice());

    let mut html = Vec::new();
    holonomy_export::write(&restored, &mut html, Format::Html, "Test").expect("HTML export");
    let html = String::from_utf8(html).unwrap();
    assert!(html.contains("data:image/png;base64,"));
    assert!(!html.contains("<svg"));

    let mut pdf = Vec::new();
    holonomy_export::write(&restored, &mut pdf, Format::Pdf, "Test").expect("PDF export");
    assert!(pdf.starts_with(b"%PDF-"));
}
