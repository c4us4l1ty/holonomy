//! A separate crate can use the store without invoking the CLI binary.

use holonomy_cli::document::{DocumentSessionOptions, DocumentSessionState};
use holonomy_cli::store::{sector_index, split_payload};
use holonomy_core::unlock::Topology;
use holonomy_crypto::ParamsClass;

#[test]
fn external_store_api_is_available_without_the_binary() {
    assert_ne!(
        DocumentSessionState::Ready,
        DocumentSessionState::RecoveryRequired
    );
    let document = b"editor-owned content";
    assert_eq!(split_payload(document), vec![document.as_slice()]);
    let sector = sector_index(&[0x5a; 32], 0, 0);
    assert!(sector < holonomy_core::layout::ZONE_B_SECTORS);
    assert_eq!(sector, sector_index(&[0x5a; 32], 0, 0));
}

#[test]
fn external_document_sessions_use_production_kdf_profile() {
    let options = DocumentSessionOptions::new(0, true, Topology::UmbilicalUsb);
    assert_eq!(options.params(), ParamsClass::Legacy);
}
