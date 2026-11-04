//! A separate crate can use the store without invoking the CLI binary.

use holonomy_cli::store::{sector_index, split_payload};

#[test]
fn external_store_api_is_available_without_the_binary() {
    let document = b"editor-owned content";
    assert_eq!(split_payload(document), vec![document.as_slice()]);
    let sector = sector_index(&[0x5a; 32], 0, 0);
    assert!(sector < holonomy_core::layout::ZONE_B_SECTORS);
    assert_eq!(sector, sector_index(&[0x5a; 32], 0, 0));
}
