//! Experimental Wordcraft document serialization inside Holonomy containers.
//!
//! The version marker is inside the encrypted payload, not a container header.
//! Existing nonempty CLI documents are not automatically imported as Wordcraft
//! data. Wordcraft's in-memory model and its internal allocations are not wiped
//! by this crate; this is not a complete secure editor or a stable file format.

use std::io::{self, Write};

use holonomy_cli::document::DocumentSession;
use holonomy_cli::store::SaveReport;
use holonomy_core::layout::PAYLOAD_SLOTS;
use holonomy_session::seal::PAYLOAD_PLAINTEXT_PER_SLOT;
use wordcraft_doc::Document;
use zeroize::Zeroizing;

const MAGIC: &[u8] = b"HOLONOMY-WORDCRAFT\0";
const VERSION: u8 = 1;
/// The maximum serialized payload that fits in the current container format.
pub const MAX_DOCUMENT_BYTES: usize = PAYLOAD_SLOTS * PAYLOAD_PLAINTEXT_PER_SLOT;

/// A document could not be encoded, decoded, or saved without loss.
#[derive(Debug)]
pub enum BridgeError {
    /// Nonempty payload is not a Wordcraft document in this format.
    UnrecognizedPayload,
    /// The encoded format version is not supported by this build.
    UnsupportedVersion(u8),
    /// Serialized content exceeds the container's payload capacity.
    DocumentTooLarge,
    /// Serialization failed, without exposing document content in the error.
    Serialization,
    /// The payload is not a valid serialized Wordcraft document.
    InvalidDocument,
    /// A serialization round trip changed the document.
    RoundTripMismatch,
    /// The underlying session refused or failed the save.
    Save(String),
}

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnrecognizedPayload => {
                f.write_str("payload is not a Holonomy Wordcraft document")
            }
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported Wordcraft payload version {version}")
            }
            Self::DocumentTooLarge => f.write_str("serialized document exceeds container capacity"),
            Self::Serialization => f.write_str("could not serialize the Wordcraft document"),
            Self::InvalidDocument => f.write_str("invalid Wordcraft document payload"),
            Self::RoundTripMismatch => {
                f.write_str("Wordcraft serialization changed the document; save refused")
            }
            Self::Save(message) => write!(f, "session save failed: {message}"),
        }
    }
}

impl std::error::Error for BridgeError {}

struct BoundedWriter {
    bytes: Zeroizing<Vec<u8>>,
    limit: usize,
    exceeded: bool,
}

impl Write for BoundedWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if data.len() > self.limit.saturating_sub(self.bytes.len()) {
            self.exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "document exceeds payload limit",
            ));
        }
        self.bytes.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Encode a Wordcraft document to a versioned, bounded plaintext payload.
///
/// The returned buffer is wiped on drop. Serialization and validation can make
/// additional plaintext copies; Wordcraft does not promise memory zeroization.
pub fn encode(document: &Document) -> Result<Zeroizing<Vec<u8>>, BridgeError> {
    let mut writer = BoundedWriter {
        bytes: Zeroizing::new(Vec::new()),
        limit: MAX_DOCUMENT_BYTES,
        exceeded: false,
    };
    writer
        .write_all(MAGIC)
        .map_err(|_| BridgeError::DocumentTooLarge)?;
    writer
        .write_all(&[VERSION])
        .map_err(|_| BridgeError::DocumentTooLarge)?;
    if serde_json::to_writer(&mut writer, document).is_err() {
        return Err(if writer.exceeded {
            BridgeError::DocumentTooLarge
        } else {
            BridgeError::Serialization
        });
    }
    let restored = decode(&writer.bytes)?;
    if !document.same_content(&restored) {
        return Err(BridgeError::RoundTripMismatch);
    }
    Ok(writer.bytes)
}

/// Decode a Wordcraft payload. An empty provisioned container is a blank document.
///
/// Other legacy CLI payloads are rejected rather than guessed or silently converted.
pub fn decode(payload: &[u8]) -> Result<Document, BridgeError> {
    if payload.is_empty() {
        return Ok(Document::new());
    }
    if payload.len() > MAX_DOCUMENT_BYTES {
        return Err(BridgeError::DocumentTooLarge);
    }
    let suffix = payload
        .strip_prefix(MAGIC)
        .ok_or(BridgeError::UnrecognizedPayload)?;
    let (&version, json) = suffix.split_first().ok_or(BridgeError::InvalidDocument)?;
    if version != VERSION {
        return Err(BridgeError::UnsupportedVersion(version));
    }
    serde_json::from_slice(json).map_err(|_| BridgeError::InvalidDocument)
}

/// Serialize and save a Wordcraft document using the session's next cursor.
///
/// Encoding errors do not call `save`. A store error may occur after replacement;
/// inspect the session state and follow its recovery instructions before retrying.
pub fn save(session: &mut DocumentSession, document: &Document) -> Result<SaveReport, BridgeError> {
    let payload = encode(document)?;
    session.save(&payload).map_err(BridgeError::Save)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versioned_document_round_trip_preserves_content() {
        let mut document = Document::from_text("First paragraph\nSecond paragraph");
        document.core.title = "A title".into();
        let encoded = encode(&document).expect("encode");
        let decoded = decode(&encoded).expect("decode");
        assert!(document.same_content(&decoded));
        assert!(encoded.starts_with(MAGIC));
    }

    #[test]
    fn invalid_and_unknown_payloads_are_not_guessed() {
        assert!(matches!(
            decode(b"plain old CLI text"),
            Err(BridgeError::UnrecognizedPayload)
        ));
        let mut unknown = MAGIC.to_vec();
        unknown.push(VERSION + 1);
        assert!(matches!(
            decode(&unknown),
            Err(BridgeError::UnsupportedVersion(_))
        ));
        assert!(matches!(decode(MAGIC), Err(BridgeError::InvalidDocument)));
        let mut invalid = MAGIC.to_vec();
        invalid.push(VERSION);
        invalid.extend_from_slice(b"not json");
        assert!(matches!(
            decode(&invalid),
            Err(BridgeError::InvalidDocument)
        ));
    }

    #[test]
    fn empty_container_is_a_blank_document() {
        assert!(decode(&[]).expect("blank").same_content(&Document::new()));
    }

    #[cfg(unix)]
    #[test]
    fn document_survives_a_container_session_round_trip() {
        use holonomy_cli::document::{DocumentSession, DocumentSessionOptions};
        use holonomy_cli::store::pad::DerivedPad;
        use holonomy_core::unlock::{SealedPassphrase, Topology};

        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("document");
        let kappa = [0x43; 32];
        let mut session = DocumentSession::create(
            &path,
            Box::new(DerivedPad::new()),
            SealedPassphrase::new("test passphrase").expect("valid"),
            kappa,
            DocumentSessionOptions::new(0, true, Topology::UmbilicalUsb),
        )
        .expect("create");
        let mut document = Document::from_text("Hello, Wordcraft!\nSecond paragraph.");
        document.core.title = "A title".into();
        save(&mut session, &document).expect("save");
        assert_eq!(session.cursor(), 1);
        drop(session);

        let (_, plaintext) = DocumentSession::open(
            &path,
            Box::new(DerivedPad::new()),
            SealedPassphrase::new("test passphrase").expect("valid"),
            kappa,
            DocumentSessionOptions::new(1, false, Topology::UmbilicalUsb),
        )
        .expect("open");
        let decoded = decode(plaintext.as_slice()).expect("decode");
        assert!(document.same_content(&decoded));
    }

    #[test]
    fn bounded_writer_refuses_overflow_without_appending() {
        let mut writer = BoundedWriter {
            bytes: Zeroizing::new(Vec::new()),
            limit: 3,
            exceeded: false,
        };
        writer.write_all(b"abc").expect("fits");
        assert!(writer.write_all(b"d").is_err());
        assert!(writer.exceeded);
        assert_eq!(writer.bytes.as_slice(), b"abc");
    }
}
