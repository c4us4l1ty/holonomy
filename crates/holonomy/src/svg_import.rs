//! SVG admission for the *unsealed desktop driver*, never the document or sealed paint loop.
//!
//! A document stores PNG bytes and the sealed renderer reads only PNG. We parse SVG exactly once on
//! desktop insertion, rasterise into a bounded buffer, and insert the resulting PNG through the
//! existing image path. There is no SVG parser, XML input, font lookup, external resource resolver,
//! network access or curve evaluation in the sealed binary or in a repaint.
//!
//! The preflight is deliberately stricter than SVG: no DTD/entities, script, foreignObject, text
//! (the fontless renderer cannot draw it), images, filters, CSS stylesheets, animation or <use>.
//! An unsupported construct is an error, *not* an invisible element in a seemingly successful
//! import. Internal paint-server references such as `fill="url(#gradient)"` are allowed. The usvg
//! image resolver is also set to deny *both* data and string URLs: a future preflight relaxation
//! must not accidentally turn a relative href into a file read (the usvg default does precisely
//! that). No SVGZ: decompression before a byte cap would defeat the cap.

use std::fs::OpenOptions;
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use resvg::{tiny_skia, usvg};
use zeroize::Zeroizing;

/// Bounds on adversarial input, rather than limits on the size of a user's *document*.
pub const MAX_SVG_BYTES: usize = 256 * 1024;
pub const MAX_ELEMENTS: usize = 4096;
pub const MAX_DEPTH: usize = 32;
pub const MAX_SIDE: u32 = 1600;
pub const MAX_PIXELS: u64 = 2_000_000;
pub const MAX_PNG_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_TEXT_BYTES: usize = 8 * 1024 * 1024;

/// A capped read for *all* desktop imports. No `fs::read` of untrusted documents.
///
/// Checking metadata first makes a huge regular file cheap to refuse; `take(limit + 1)` is the
/// second check for files that grow between stat and read. Non-files are refused so a path to a
/// device or FIFO cannot hang the UI on a read. This is desktop-only: the sealed editor never opens
/// an import path, and this module isn't linked into the default feature set.
pub fn read_file(path: &Path, limit: usize) -> Result<Zeroizing<Vec<u8>>, String> {
    // A FIFO opened for reading ordinarily blocks before metadata can reject it. O_NONBLOCK
    // makes even a user-selected pipe a cheap refusal rather than a hung editor.
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| format!("Could not open {}: {e}", path.display()))?;
    let metadata = file
        .metadata()
        .map_err(|e| format!("Could not stat {}: {e}", path.display()))?;
    if !metadata.is_file() {
        return Err("Choose a regular file, not a directory, device or pipe.".into());
    }
    if metadata.len() > limit as u64 {
        return Err(format!("File exceeds the {} KiB import limit.", limit / 1024));
    }
    read_capped(file, limit).map_err(|e| format!("Could not read {}: {e}", path.display()))
}

fn read_capped<R: Read>(reader: R, limit: usize) -> io::Result<Zeroizing<Vec<u8>>> {
    let mut bytes = Zeroizing::new(Vec::new());
    reader
        .take((limit as u64).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "file exceeds import limit"));
    }
    Ok(bytes)
}

/// Load a user's PNG or SVG into the existing PNG image pipeline. Extension is a format request,
/// *not* proof of content: the PNG decoder or the XML parser validates all bytes before insertion.
pub fn load_image(path: &Path) -> Result<Zeroizing<Vec<u8>>, String> {
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    if ext.eq_ignore_ascii_case("png") {
        return read_file(path, MAX_PNG_BYTES);
    }
    if ext.eq_ignore_ascii_case("svg") {
        let source = read_file(path, MAX_SVG_BYTES)?;
        return rasterize_svg(&source);
    }
    Err("Choose a .png or .svg file (SVGZ and linked resources are not supported).".into())
}

/// Convert a bounded, self-contained SVG to PNG. The returned pixels are the *only* form saved in
/// the encrypted document. `Zeroizing` scrubs both the input read by `load_image` and the temporary
/// RGBA/PNG buffers; only resvg's parsed geometry is left to Rust's normal allocator on drop.
pub fn rasterize_svg(source: &[u8]) -> Result<Zeroizing<Vec<u8>>, String> {
    if source.is_empty() || source.len() > MAX_SVG_BYTES {
        return Err(format!("SVG must be 1..={} KiB.", MAX_SVG_BYTES / 1024));
    }
    let text = std::str::from_utf8(source).map_err(|_| "SVG must be UTF-8 XML (not SVGZ).")?;
    // roxmltree defaults to allow_dtd=false. usvg::Tree::from_str, in contrast, *enables* DTD,
    // so pass the validated tree to from_xmltree instead of making it parse the bytes a second time.
    let doc = roxmltree::Document::parse(text).map_err(|e| format!("Invalid SVG XML: {e}"))?;
    preflight(&doc)?;

    let mut options = usvg::Options::default();
    options.resources_dir = None;
    options.image_href_resolver = usvg::ImageHrefResolver {
        resolve_data: Box::new(|_, _, _| None),
        resolve_string: Box::new(|_, _| None),
    };
    let tree = usvg::Tree::from_xmltree(&doc, &options)
        .map_err(|e| format!("Unsupported SVG geometry: {e}"))?;
    let (width, height) = target_size(tree.size().width(), tree.size().height())?;
    let byte_len = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or("SVG dimensions overflow the raster budget")?;
    let mut rgba = Zeroizing::new(vec![0u8; byte_len]);
    {
        let mut pixmap = tiny_skia::PixmapMut::from_bytes(&mut rgba, width, height)
            .ok_or("SVG raster dimensions are invalid")?;
        let sx = width as f32 / tree.size().width();
        let sy = height as f32 / tree.size().height();
        if !sx.is_finite() || !sy.is_finite() || sx <= 0.0 || sy <= 0.0 {
            return Err("SVG viewBox produces a non-finite raster scale.".into());
        }
        resvg::render(&tree, tiny_skia::Transform::from_scale(sx, sy), &mut pixmap);
    }

    // tiny-skia stores premultiplied RGBA; PNG requires straight RGBA. Do it *in place*, rather
    // than Pixmap::encode_png's full-size temporary copy (up to another 8 MiB of plaintext).
    for pixel in rgba.chunks_exact_mut(4) {
        let alpha = u32::from(pixel[3]);
        for channel in &mut pixel[..3] {
            *channel = if alpha == 0 {
                0
            } else {
                (u32::from(*channel) * 255 / alpha).min(255) as u8
            };
        }
    }
    let mut output = Zeroizing::new(Vec::new());
    {
        let mut encoder = png::Encoder::new(&mut *output, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|e| format!("Could not encode SVG as PNG: {e}"))?;
        writer
            .write_image_data(&rgba)
            .map_err(|e| format!("Could not encode SVG as PNG: {e}"))?;
    }
    if output.len() > MAX_PNG_BYTES {
        return Err("Rasterised SVG exceeds the 16 MiB PNG limit.".into());
    }
    Ok(output)
}

/// Fit a small icon at a useful size without upscaling a page-size graphic, and bound both the
/// longest edge and total pixels *before* allocating. PNG catalog dimensions are u16, too.
fn target_size(w: f32, h: f32) -> Result<(u32, u32), String> {
    if !w.is_finite() || !h.is_finite() || w <= 0.0 || h <= 0.0 || w > 1_000_000.0 || h > 1_000_000.0 {
        return Err("SVG needs finite, positive dimensions no larger than 1,000,000.".into());
    }
    let (w, h) = (f64::from(w), f64::from(h));
    let scale = (256.0 / w.max(h))
        .max(1.0)
        .min(f64::from(MAX_SIDE) / w)
        .min(f64::from(MAX_SIDE) / h)
        .min((MAX_PIXELS as f64 / (w * h)).sqrt());
    let width = (w * scale).round().clamp(1.0, f64::from(MAX_SIDE)) as u32;
    let height = (h * scale).round().clamp(1.0, f64::from(MAX_SIDE)) as u32;
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err("SVG raster would exceed the 2-million-pixel limit.".into());
    }
    Ok((width, height))
}

/// Allowlist of static vector primitives. Deliberately exclude elements usvg would silently drop,
/// and elements that duplicate/expand a subtree or run a filter over many intermediate rasters.
fn preflight(doc: &roxmltree::Document<'_>) -> Result<(), String> {
    let root = doc.root_element();
    if root.tag_name().name() != "svg"
        || !matches!(root.tag_name().namespace(), None | Some("http://www.w3.org/2000/svg"))
    {
        return Err("Expected an SVG root element.".into());
    }
    let mut count = 0;
    for node in doc.descendants().filter(|n| n.is_element()) {
        count += 1;
        if count > MAX_ELEMENTS {
            return Err(format!("SVG has more than {MAX_ELEMENTS} elements."));
        }
        if node.ancestors().count() > MAX_DEPTH {
            return Err(format!("SVG nesting exceeds {MAX_DEPTH} levels."));
        }
        if !matches!(node.tag_name().namespace(), None | Some("http://www.w3.org/2000/svg")) {
            return Err("SVG contains an element in an unsupported namespace.".into());
        }
        let tag = node.tag_name().name();
        if !matches!(
            tag,
            "svg" | "g" | "defs" | "path" | "rect" | "circle" | "ellipse" | "line"
                | "polyline" | "polygon" | "linearGradient" | "radialGradient" | "stop"
                | "clipPath" | "title" | "desc"
        ) {
            return Err(format!("SVG <{tag}> is not in the safe static-vector subset."));
        }
        for attr in node.attributes() {
            let name = attr.name();
            let value = attr.value();
            if name.get(..2).is_some_and(|prefix| prefix.eq_ignore_ascii_case("on")) {
                return Err("SVG event handlers are not supported.".into());
            }
            if name == "href" && (!value.starts_with('#') || value.len() < 2) {
                return Err("SVG may only reference its own paint servers.".into());
            }
            if name == "base" && attr.namespace() == Some("http://www.w3.org/XML/1998/namespace") {
                return Err("SVG xml:base is not supported.".into());
            }
            if matches!(name, "filter" | "mask" | "marker-start" | "marker-mid" | "marker-end") {
                return Err(format!("SVG {name} effects are not supported."));
            }
            if !internal_urls_only(value) {
                return Err("SVG external URLs and CSS imports are not supported.".into());
            }
            if name == "style" && !static_inline_style(value) {
                return Err("SVG inline CSS contains an unsupported property.".into());
            }
        }
    }
    Ok(())
}

/// Inline CSS must only style static geometry. Without a property allowlist a `<path>` could
/// request a filter, mask or external resource *without* using a banned SVG element.
fn static_inline_style(style: &str) -> bool {
    for declaration in style.split(';').map(str::trim).filter(|part| !part.is_empty()) {
        let Some((property, value)) = declaration.split_once(':') else { return false };
        if value.trim().is_empty() || !matches!(
            property.trim(),
            "fill" | "fill-rule" | "fill-opacity" | "stroke" | "stroke-width"
                | "stroke-linecap" | "stroke-linejoin" | "stroke-miterlimit"
                | "stroke-dasharray" | "stroke-dashoffset" | "stroke-opacity"
                | "opacity" | "color" | "stop-color" | "stop-opacity"
                | "clip-path" | "clip-rule" | "display" | "visibility"
        ) {
            return false;
        }
    }
    true
}

/// URL syntax used in gradients/clip paths. Any non-fragment reference fails closed; disabling
/// usvg's image resolver above is the independent, lower-level safeguard against file reads.
fn internal_urls_only(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    if lower.contains("@import") {
        return false;
    }
    let mut tail = lower.as_str();
    while let Some(start) = tail.find("url(") {
        let remaining = &tail[start + 4..];
        let Some(end) = remaining.find(')') else { return false };
        let fragment = remaining[..end].trim().trim_matches(&['\'', '"'][..]);
        if !fragment.starts_with('#')
            || fragment.len() < 2
            || !fragment[1..].bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.' | b':'))
        {
            return false;
        }
        tail = &remaining[end + 1..];
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="32" viewBox="0 0 64 32"><defs><linearGradient id="shade"><stop stop-color="#e53935"/><stop offset="1" stop-color="#e53935"/></linearGradient></defs><path fill="url(#shade)" d="M4 4 H60 V28 H4 Z"/></svg>"##;

    #[test]
    fn a_static_vector_is_png_with_the_right_size_and_alpha() {
        let output = rasterize_svg(SVG.as_bytes()).unwrap();
        let header = holonomy_image::read_header(&output).unwrap();
        assert_eq!((header.width, header.height), (256, 128));
        let mut decoded = vec![0; holonomy_image::decoded_len(&header)];
        holonomy_image::decode(&output, &mut decoded).unwrap();
        assert_eq!(decoded[3], 0, "outside the path is transparent");
        let center = ((64 * 256 + 128) * 4) as usize;
        assert!(decoded[center] > 200 && decoded[center + 1] < 100);
        assert_eq!(decoded[center + 3], 255);
    }

    #[test]
    fn external_files_scripts_text_and_dtd_are_refused_instead_of_silently_dropped() {
        for bad in [
            r#"<svg xmlns="http://www.w3.org/2000/svg"><image href="/etc/passwd"/></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg" onload="alert(1)"/>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg"><script>bad()</script></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg"><text>not supported</text></svg>"#,
            r##"<svg xmlns="http://www.w3.org/2000/svg"><use href="#shape"/></svg>"##,
            r#"<svg xmlns="http://www.w3.org/2000/svg"><path style="fill:url(file:///etc/passwd)"/></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg"><rect filter="url(#shadow)"/></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg"><rect style="filter:url(#shadow)"/></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg"><rect mask="url(#shape)"/></svg>"#,
            r#"<!DOCTYPE svg [<!ENTITY x SYSTEM "file:///etc/passwd">]><svg xmlns="http://www.w3.org/2000/svg"/>"#,
        ] {
            assert!(rasterize_svg(bad.as_bytes()).is_err(), "accepted: {bad}");
        }
    }

    #[test]
    fn bounded_reads_and_geometry_refuse_excessive_resources() {
        assert!(read_capped(std::io::Cursor::new(vec![b'x'; MAX_SVG_BYTES + 1]), MAX_SVG_BYTES).is_err());
        assert!(rasterize_svg(&vec![b'x'; MAX_SVG_BYTES + 1]).is_err());
        assert!(target_size(f32::INFINITY, 12.0).is_err());
        assert!(target_size(1_000_001.0, 1.0).is_err());
        assert_eq!(target_size(24.0, 24.0).unwrap(), (256, 256));
        assert!(u64::from(target_size(999_999.0, 999_999.0).unwrap().0).pow(2) <= MAX_PIXELS);
        let deep = format!("<svg>{}</svg>", "<g>".repeat(MAX_DEPTH) + &"</g>".repeat(MAX_DEPTH));
        assert!(rasterize_svg(deep.as_bytes()).is_err());
    }

    #[test]
    fn inline_css_is_static_vector_only() {
        assert!(static_inline_style("fill:#1a73e8; stroke: black; stroke-width: 2"));
        assert!(static_inline_style("fill:url(#shade); opacity:0.75"));
        for bad in ["filter:url(#blur)", "mask:url(#mask)", "font-family:Arial", "fill", "fill:"] {
            assert!(!static_inline_style(bad), "accepted CSS: {bad}");
        }
    }

    #[test]
    fn file_inputs_are_bounded_and_a_pipe_cannot_block_before_validation() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let dir = std::env::temp_dir().join(format!(
            "holonomy-svg-import-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&dir).unwrap();
        let huge = dir.join("huge.svg");
        std::fs::write(&huge, vec![b'x'; MAX_SVG_BYTES + 1]).unwrap();
        assert!(read_file(&huge, MAX_SVG_BYTES).is_err());
        let pipe = dir.join("pipe");
        let path = CString::new(pipe.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        assert!(read_file(&pipe, MAX_SVG_BYTES).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn only_internal_paint_references_are_allowed() {
        for ok in ["url(#ink)", "fill:url( '#gradient-2' )", "#ink"] {
            assert!(internal_urls_only(ok), "{ok}");
        }
        for bad in ["url(https://example.org/x)", "url(file.svg#ink)", "@import 'file.css'", "url(data:image/png;base64,AAAA)", "url(#)" ] {
            assert!(!internal_urls_only(bad), "{bad}");
        }
    }
}
