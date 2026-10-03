//! Write a MessagePack-encoded `SectionContent` for the frontend to decode.
//!
//! Run: cargo run -p holonomy-shell --example gen_wire_fixture
//!
//! # Why this exists
//!
//! The boot path and the `get_section` path return the *same* Rust type over the *same*
//! `rmp-serde` encoding, and the frontend decodes both with the *same* strict check —
//! `content_zstd` must be a `Uint8Array`, or the decoder throws rather than repairing.
//!
//! That symmetry is a claim, and a claim about a wire format is exactly the sort of thing
//! that is true on the machine that wrote it and false somewhere else. The in-engine run
//! found it: `get_section` delivered `content_zstd` as a plain object while
//! `get_document_boot` delivered a `Uint8Array`, from the same command family, on the same
//! engine, in the same window.
//!
//! So the format is pinned from both ends instead of inferred: this writes what
//! `rmp-serde` actually produces, and `app/test/wire.ts` asserts what
//! `@msgpack/msgpack` makes of it. If the encoder's representation of `Vec<u8>` changes,
//! that test fails here rather than as a hydration error in a real window.

use holonomy_shell_lib::bridge::{BootPayload, ManifestSection, SectionContent};

/// Two sections of fixture prose, big enough that the array-versus-`bin` size difference
/// is unmistakable rather than a rounding error.
fn fixture_json(section: usize) -> serde_json::Value {
    serde_json::json!({
        "type": "doc",
        "content": (0..8).map(|p| serde_json::json!({
            "type": "paragraph",
            "content": [{"type": "text", "text": format!(
                "Section {section} paragraph {p}. {}",
                "lorem ipsum dolor sit amet consectetur adipiscing elit ".repeat(4)
            )}]
        })).collect::<Vec<_>>()
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // From the manifest, not the cwd: `cargo run` executes with the *workspace* root as
    // the working directory, so a relative path resolves outside the repository and fails
    // with a permission error that says nothing about what was wrong.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../app/test/fixtures");
    std::fs::create_dir_all(&dir)?;

    let frames: Vec<Vec<u8>> = (0..2).map(|s| holonomy_core::store::encode(&fixture_json(s)).unwrap()).collect();
    let content = SectionContent { id: "wire-fixture".into(), content_zstd: frames[0].clone() };

    let bytes = rmp_serde::to_vec_named(&content)?;
    std::fs::write(dir.join("rust-msgpack-section.bin"), &bytes)?;

    // The first byte of the MessagePack stream is the map header, and the byte after it
    // is the value tag for the first field. Written to a text file so a failure can be
    // diagnosed from the tags alone, without a MessagePack reader in the failing process.
    println!(
        "wrote rust-msgpack-section.bin: {} bytes; first two tags are {:02x} {:02x}",
        bytes.len(),
        bytes[0],
        bytes[1]
    );

    // `bin` is 0xc4/0xc5/0xc6 (with a length prefix); `array` is 0x90..0x9f / 0xdc /
    // 0xdd. Which one this is decides whether the frontend's `instanceof Uint8Array`
    // check passes, so it is recorded as a value the test asserts on rather than left to
    // be inferred from a length.
    let tag = bytes
        .iter()
        .copied()
        .find(|b| *b == 0xc4 || *b == 0xc5 || *b == 0xc6 || (*b >= 0x90 && *b <= 0x9f))
        .unwrap_or(0);
    std::fs::write(
        dir.join("rust-msgpack-section.tag"),
        format!("{tag:02x}\n"),
    )?;
    println!(
        "value tag {:02x}: {}",
        tag,
        match tag {
            0xc4..=0xc6 => "MessagePack bin, which decodes to a Uint8Array",
            0x90..=0x9f => "MessagePack array, which decodes to a plain number[]",
            _ => "neither bin nor a short array",
        }
    );

    // The whole boot payload, because that is the message the frontend decodes first and
    // the one whose size decides whether MessagePack was worth choosing.
    let calibration = holonomy_core::geometry::GeometryCalibration::default();
    let boot = BootPayload {
        document_id: "wire-fixture".into(),
        title: "Wire fixture".into(),
        calibration,
        sections: (0..8)
            .map(|i| ManifestSection {
                id: format!("s{i}"),
                order_key: i as u64 * 1024,
                title: None,
                word_count: 400 + i as u32,
                mark_count: i as u32,
                char_count: 2000 + i as u32,
                block_count: 8,
                created_at: 0,
                updated_at: 0,
            })
            .collect(),
        visible: frames
            .iter()
            .enumerate()
            .map(|(i, f)| SectionContent { id: format!("s{i}"), content_zstd: f.clone() })
            .collect(),
        focused_section_id: None,
        scroll_top: None,
    };
    let boot_bytes = rmp_serde::to_vec_named(&boot)?;
    std::fs::write(dir.join("rust-msgpack-boot.bin"), &boot_bytes)?;
    println!(
        "wrote rust-msgpack-boot.bin: {} bytes for 2 sections carrying {} compressed bytes",
        boot_bytes.len(),
        frames.iter().map(|f| f.len()).sum::<usize>()
    );

    Ok(())
}
