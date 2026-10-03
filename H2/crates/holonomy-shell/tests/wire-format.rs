//! The MessagePack wire format, pinned from the Rust side.
//!
//! `app/test/wire.ts` makes the same claim from the other end: it decodes fixtures written
//! by `cargo run -p holonomy-shell --example gen_wire_fixture` and asserts the frontend's
//! decoders will accept them. These tests read the same fixtures and assert what the
//! encoder produced, so dropping `#[serde(with = "serde_bytes")]` from
//! `SectionContent` fails here and not only in a real window.
//!
//! # What this is guarding
//!
//! `rmp-serde` serialises a bare `Vec<u8>` as a MessagePack **array of numbers**.
//! `@msgpack/msgpack` decodes an array to `number[]`, and both frontend decoders check
//! `instanceof Uint8Array` and throw rather than repair. So section content had never
//! decoded over the bridge. The `#[ts(type = "Uint8Array")]` annotation on the field
//! says what the TypeScript is and does nothing to what the encoder writes, which is why
//! the type was right and the wire was not.
//!
//! The bug survived because `test/boot.ts` encoded its own fixture with
//! `@msgpack/msgpack`'s `encode`, which *does* write a `Uint8Array` as `bin`. The test
//! agreed with itself and with neither the real encoder nor the real decoder.
//!
//! Run: cargo test -p holonomy-shell --test wire-format

use holonomy_shell_lib::bridge::{BootPayload, ManifestSection, SectionContent};
use std::path::Path;

/// A boot payload with the same shape as `gen_wire_fixture`'s, built in memory.
///
/// Two sections with content and eight manifest rows, so `visible` and `sections` are both
/// non-empty. Live rather than loaded from the fixture, for the reason given on
/// `section_content_is_encoded_as_messagepack_bin`.
fn fixture_boot() -> BootPayload {
    let frames: Vec<Vec<u8>> = (0..2)
        .map(|s| holonomy_core::store::encode(&serde_json::json!({
            "type": "doc",
            "content": (0..8).map(|p| serde_json::json!({
                "type": "paragraph",
                "content": [{"type": "text", "text": format!("section {s} paragraph {p}")}]
            })).collect::<Vec<_>>()
        })))
        .collect::<std::result::Result<Vec<_>, _>>()
        .expect("encodes");
    BootPayload {
        document_id: "wire-fixture".into(),
        title: "Wire fixture".into(),
        calibration: holonomy_core::geometry::GeometryCalibration::default(),
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
    }
}

fn fixtures() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../app/test/fixtures")
}

/// Skip one MessagePack value, returning how many bytes it occupied.
///
/// Enough of the format to walk the two-field `SectionContent` map: fixstr, str8/16/32,
/// bin8/16/32, fixarray, array16/32, nil/bool/int and fixmap. Written rather than
/// borrowed because a serde round trip cannot detect this defect at all — a `Vec<u8>`
/// reads back from an array of numbers perfectly happily — so the question is only
/// answerable by looking at the bytes.
fn skip(bytes: &[u8], at: usize) -> usize {
    let tag = bytes[at];
    // One tag byte plus the payload. Written `1 + n`; it was `n + 1 + n` in the first
    // version, which over-ran by exactly the payload length and made the walk land on a
    // character inside "content_zstd". It produced a plausible-looking tag rather than
    // an out-of-range index, so nothing said the walk was wrong.
    let take = |n: usize| 1 + n;
    match tag {
        0x00..=0x7f => take(0),                 // positive fixint
        0xe0..=0xff => take(0),                 // negative fixint
        0xc0 | 0xc2 | 0xc3 => take(0),          // nil, false, true
        0xcc => take(1),                        // uint8
        0xcd => take(2),                        // uint16
        0xce => take(4),                        // uint32
        0xcf => take(8),                        // uint64
        0xd0 => take(1),                        // int8
        0xd1 => take(2),                        // int16
        0xd2 => take(4),                        // int32
        0xd3 => take(8),                        // int64
        0xa0..=0xbf => take((tag & 0x1f) as usize), // fixstr
        0xd9 => take(bytes[at + 1] as usize),   // str8
        0xda => take(u16::from_be_bytes([bytes[at + 1], bytes[at + 2]]) as usize), // str16
        0xdb => take(u32::from_be_bytes([
            bytes[at + 1],
            bytes[at + 2],
            bytes[at + 3],
            bytes[at + 4],
        ]) as usize), // str32
        0xc4 => take(bytes[at + 1] as usize),   // bin8
        0xc5 => take(u16::from_be_bytes([bytes[at + 1], bytes[at + 2]]) as usize), // bin16
        0xc6 => take(u32::from_be_bytes([
            bytes[at + 1],
            bytes[at + 2],
            bytes[at + 3],
            bytes[at + 4],
        ]) as usize), // bin32
        0x90..=0x9f => {
            let mut at = at + 1 + (tag & 0x0f) as usize;
            for _ in 0..(tag & 0x0f) {
                at = at + skip(bytes, at);
            }
            at
        }
        0x80..=0x8f => {
            let mut at = at + 1;
            for _ in 0..(tag & 0x0f) {
                at = at + skip(bytes, at); // key
                at = at + skip(bytes, at); // value
            }
            at
        }
        other => panic!("fixture uses an unhandled MessagePack tag {other:#04x}"),
    }
}

/// The value tag the encoder wrote for `content_zstd`.
///
/// Walks the map by field rather than by byte offset. The first version hard-coded the
/// offsets, which were off by two — it read the length byte of `id`'s *value* where it
/// meant `id`'s key — and then "fixed" the expectation to match what it had read. A walk
/// cannot be wrong that way: if the id or a field name changes length, it still finds the
/// second field.
fn section_content_tag(bytes: &[u8]) -> u8 {
    assert_eq!(bytes[0], 0x82, "expected a two-field map, got tag {:#04x}", bytes[0]);
    let mut at = 1;
    for field in 0..2 {
        at += skip(bytes, at); // key
        let value = at;
        if field == 1 {
            return bytes[value];
        }
        at += skip(bytes, at);
    }
    unreachable!("a two-field map has a second field")
}

#[test]
fn section_content_is_encoded_as_messagepack_bin() {
    // Encoded *here*, not read from the committed fixture.
    //
    // The first version read `rust-msgpack-section.bin` and asserted on its bytes, which
    // passed with `#[serde(with = "serde_bytes")]` deleted from the struct — because the
    // fixture on disk still held the good bytes. A guard that reads a checked-in artifact
    // asserts that the artifact is unchanged, not that the code produces it, and the code
    // is the thing that regressed. Confirmed by mutation: removing the attribute left all
    // four tests green here while three of them failed on the frontend side.
    let live = SectionContent {
        id: "wire-fixture".into(),
        content_zstd: (0..300u16).map(|i| (i % 251) as u8).collect(),
    };
    let bytes = rmp_serde::to_vec_named(&live).expect("encodes");
    let tag = section_content_tag(&bytes);

    // 0xc4/0xc5/0xc6 are bin8/16/32. 0x90..0x9f, 0xdc and 0xdd are arrays, which is what
    // a bare `Vec<u8>` produces and what `@msgpack/msgpack` decodes to `number[]`.
    assert!(
        (0xc4..=0xc6).contains(&tag),
        "content_zstd is tagged {tag:#04x}, not a MessagePack bin. rmp-serde serialises a bare \
         Vec<u8> as an array of numbers, so SectionContent::content_zstd needs \
         #[serde(with = \"serde_bytes\")] or every section payload decodes as number[] and the \
         frontend refuses it"
    );
}

#[test]
fn a_live_boot_payload_encodes_every_section_as_bin() {
    // Same reasoning as above, applied to the message that goes out first. `visible` holds
    // `SectionContent` values, so the attribute carries over — and it is worth pinning,
    // because "the boot payload decodes but `get_section` does not" was the shape of the
    // original symptom and nobody would guess it a second time.
    let boot = fixture_boot();
    let bytes = rmp_serde::to_vec_named(&boot).expect("encodes");
    let decoded: BootPayload = rmp_serde::from_slice(&bytes).expect("round trips");

    for section in &decoded.visible {
        let one = rmp_serde::to_vec_named(&section).expect("encodes");
        let tag = section_content_tag(&one);
        assert!(
            (0xc4..=0xc6).contains(&tag),
            "boot section {} encodes its content as {tag:#04x}, not a bin",
            section.id
        );
    }
    assert_eq!(decoded.visible.len(), 2, "the fixture should carry two sections' content");
}

#[test]
fn binary_encoding_is_smaller_than_the_array_it_replaces() {
    // The consequence, and the reason `serde_bytes` is not a detail. A MessagePack array
    // element is one byte below 128, two below 256, three below 65536; compressed data is
    // not ASCII, so it averages above 1.5 bytes per byte.
    //
    // The threshold is 1.4 rather than a round number because that is what the
    // distribution supports. Asserting 2x here would be asserting a figure chosen for
    // convenience, and it would fail the day the compressor's output shifted.
    let content: Vec<u8> = (0..4096u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8).collect();
    let as_bin = rmp_serde::to_vec_named(&SectionContent {
        id: "s".into(),
        content_zstd: content.clone(),
    })
    .expect("bin");
    let as_array = rmp_serde::to_vec_named(&serde_json::json!({
        "id": "s",
        "content_zstd": content,
    }))
    .expect("array");

    let ratio = as_array.len() as f64 / as_bin.len() as f64;
    assert!(
        ratio > 1.4,
        "the array encoding should be at least 1.4x for 4096 bytes of compressed data, got \
         {ratio:.2}x ({} as bin, {} as an array)",
        as_bin.len(),
        as_array.len()
    );
}

#[test]
fn the_payload_round_trips_back_into_section_content() {
    // The other half of "pinned from both ends": the bytes are not merely well-formed, they
    // decode into the type the command returns, with the content intact.
    let bytes = std::fs::read(fixtures().join("rust-msgpack-section.bin")).expect("fixture");
    let decoded: SectionContent = rmp_serde::from_slice(&bytes).expect("decodes as SectionContent");
    assert_eq!(decoded.id, "wire-fixture");
    assert!(
        !decoded.content_zstd.is_empty(),
        "the content should not be empty after a round trip"
    );
    let json = holonomy_core::store::decode(&decoded.id, &decoded.content_zstd)
        .expect("the content is still a zstd frame holding JSON");
    assert_eq!(json["type"], "doc");
    assert_eq!(json["content"].as_array().map(|c| c.len()), Some(8));
}

#[test]
fn a_boot_payload_round_trips_with_its_integers_intact() {
    // The geometry depends on `block_count` and the calibration's three floats. A
    // payload that decoded but lost a number would render and then estimate every height
    // wrong, with nothing reporting an error — so the integers are compared, not just the
    // structure.
    let bytes = std::fs::read(fixtures().join("rust-msgpack-boot.bin")).expect("fixture");
    let boot: BootPayload = rmp_serde::from_slice(&bytes).expect("decodes as BootPayload");
    assert_eq!(boot.document_id, "wire-fixture");
    assert_eq!(boot.sections.len(), 8);
    assert_eq!(boot.visible.len(), 2);
    assert!(
        boot.sections.iter().all(|s| s.block_count == 8),
        "block_count is what estimateHeight depends on and there is no fallback for it"
    );
    assert!(
        boot.sections.iter().enumerate().all(|(i, s)| s.order_key == i as u64 * 1024),
        "order keys must survive as integers; a float would silently reorder the document"
    );
    assert!(
        boot.calibration.px_per_100_chars > 0.0,
        "the calibration's floats must survive: {}",
        boot.calibration.px_per_100_chars
    );
    assert!(boot.scroll_top.is_none(), "a first open has no scroll offset");
    let _: ManifestSection = boot.sections[0].clone();
}