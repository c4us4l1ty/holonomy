//! Compile trusted, reviewed SVG artwork into compact 1-bit masks in `.rodata`.
//!
//! WordCraft's icon library draws geometry on a 24x24 grid. We use that original artwork under MIT
//! (third_party/wordcraft/LICENSE-MIT), render the *source files at build time* at both native 16px
//! and desktop 20px, and emit the same `u64` rows as Holonomy's existing icon format. The product
//! binary never contains SVG source, an XML parser or the vector renderer. Rasterising at 20px,
//! instead of nearest-neighbour-enlarging a 16px mask, keeps the title and navigation icons crisp.
use std::fmt::Write as _;
use std::path::PathBuf;

use resvg::{tiny_skia, usvg};

const ICONS: &[(&str, &str)] = &[
    ("DOC", "assets/icons/document.svg"),
    ("FOLDER", "assets/icons/folder.svg"),
    ("PEN", "assets/icons/pencil.svg"),
    ("ARROW_LEFT", "assets/icons/arrow-left.svg"),
    ("WC_PASTE", "assets/icons/wordcraft-paste.svg"),
    ("WC_CUT", "assets/icons/wordcraft-cut.svg"),
    ("WC_COPY", "assets/icons/wordcraft-copy.svg"),
    ("WC_SAVE", "assets/icons/wordcraft-save.svg"),
    ("WC_SEARCH", "assets/icons/wordcraft-search.svg"),
    ("WC_BULLETS", "assets/icons/wordcraft-bullets.svg"),
    ("WC_ALIGN_LEFT", "assets/icons/wordcraft-align-left.svg"),
    ("WC_ALIGN_CENTER", "assets/icons/wordcraft-align-center.svg"),
    ("WC_ALIGN_RIGHT", "assets/icons/wordcraft-align-right.svg"),
    ("WC_INDENT", "assets/icons/wordcraft-indent.svg"),
    ("WC_OUTDENT", "assets/icons/wordcraft-outdent.svg"),
    ("WC_TABLE", "assets/icons/wordcraft-table.svg"),
    ("WC_PICTURE", "assets/icons/wordcraft-picture.svg"),
    ("WC_PAGE_BREAK", "assets/icons/wordcraft-page-break.svg"),
    ("WC_NAVIGATION", "assets/icons/wordcraft-navigation.svg"),
    ("WC_THEME", "assets/icons/wordcraft-theme.svg"),
    ("WC_CLOSE", "assets/icons/wordcraft-close.svg"),
    ("WC_CHEVRON_UP", "assets/icons/wordcraft-chevron-up.svg"),
    ("WC_CHEVRON_DOWN", "assets/icons/wordcraft-chevron-down.svg"),
    ("WC_MORE", "assets/icons/wordcraft-more.svg"),
    ("WC_UNDO", "assets/icons/wordcraft-undo.svg"),
    ("WC_REDO", "assets/icons/wordcraft-redo.svg"),
];

fn main() {
    let base = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    let mut output =
        String::from("// Generated from MIT-licensed WordCraft artwork. Do not edit.\n");
    for &(name, source) in ICONS {
        println!("cargo:rerun-if-changed={source}");
        let path = base.join(source);
        let bytes = std::fs::read(&path).expect("read checked-in SVG icon");
        assert!(
            bytes.len() <= 2048,
            "trusted UI icon has grown beyond its small vector budget"
        );
        let xml = std::str::from_utf8(&bytes).expect("icon is UTF-8");
        let doc = roxmltree::Document::parse(xml).expect("icon XML is valid and has no DTD");
        assert_eq!(doc.root_element().tag_name().name(), "svg");
        // Keep this *authoring format* narrower than even the desktop import subset. Checked-in
        // WordCraft icons have only SVG and path nodes, no stylesheet, filters, linked images or text.
        for node in doc.descendants().filter(|node| node.is_element()) {
            assert!(matches!(node.tag_name().name(), "svg" | "path"));
            assert!(matches!(
                node.tag_name().namespace(),
                None | Some("http://www.w3.org/2000/svg")
            ));
            for attr in node.attributes() {
                assert!(
                    attr.namespace().is_none(),
                    "namespaced attributes are not UI art"
                );
                assert!(!attr.name().starts_with("on") && !matches!(attr.name(), "href" | "style"));
                assert!(!attr.value().to_ascii_lowercase().contains("url("));
            }
        }
        let mut options = usvg::Options::default();
        options.image_href_resolver = usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        };
        let tree = usvg::Tree::from_xmltree(&doc, &options).expect("icon SVG is valid");
        for side in [16u32, 20u32] {
            let mut pixmap = tiny_skia::Pixmap::new(side, side).expect("small icon pixmap");
            let scale = tiny_skia::Transform::from_scale(
                side as f32 / tree.size().width(),
                side as f32 / tree.size().height(),
            );
            resvg::render(&tree, scale, &mut pixmap.as_mut());
            writeln!(output, "pub static {name}_{side}: [u64; {side}] = [").unwrap();
            for row in pixmap.data().chunks_exact(side as usize * 4) {
                let mut bits = 0u64;
                for (x, pixel) in row.chunks_exact(4).enumerate() {
                    // Low threshold preserves 1px diagonals at 16px without bloating a stroke into
                    // a grey, two-pixel blur. No antialiasing work is done in the paint loop.
                    if pixel[3] >= 96 {
                        bits |= 1u64 << (63 - x);
                    }
                }
                writeln!(output, "    0x{bits:016X},").unwrap();
            }
            writeln!(output, "];\n").unwrap();
        }
    }
    let dest = PathBuf::from(std::env::var_os("OUT_DIR").expect("build output"));
    std::fs::write(dest.join("svg_icons.rs"), output).expect("write generated icon masks");
}
