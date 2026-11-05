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

/// Frontend-facing document ownership and conservative save state.
pub mod editor;
/// Limited Wordcraft editing commands bound to a container session.
pub mod engine;

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
    /// A valid JSON payload contains an unsupported or noncanonical representation.
    UnsupportedRepresentation,
    /// A serialization round trip changed the document.
    RoundTripMismatch,
    /// A container could not be created or opened.
    Session(String),
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
            Self::UnsupportedRepresentation => f.write_str(
                "Wordcraft payload contains unsupported or noncanonical data; open refused to avoid data loss",
            ),
            Self::RoundTripMismatch => {
                f.write_str("Wordcraft serialization changed the document; save refused")
            }
            Self::Session(message) => write!(f, "session could not be opened: {message}"),
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

// Compare serialization against the input without allocating another complete
// JSON document. Any ignored field or normalized representation must fail closed.
struct MatchingWriter<'a> {
    expected: &'a [u8],
    offset: usize,
}

impl Write for MatchingWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let end = self.offset.checked_add(bytes.len()).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "serialized payload differs")
        })?;
        if self.expected.get(self.offset..end) != Some(bytes) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "serialized payload differs"));
        }
        self.offset = end;
        Ok(bytes.len())
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
/// Version 1 accepts only the canonical JSON emitted by the pinned serializer;
/// unknown fields or alternate representations are rejected, not discarded.
/// This strict experimental format needs an explicit compatibility policy before
/// changing the serializer or the pinned Wordcraft document model.
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
    let document: Document = serde_json::from_slice(json).map_err(|_| BridgeError::InvalidDocument)?;
    let mut writer = MatchingWriter { expected: json, offset: 0 };
    if serde_json::to_writer(&mut writer, &document).is_err() || writer.offset != json.len() {
        return Err(BridgeError::UnsupportedRepresentation);
    }
    Ok(document)
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
    fn unknown_fields_and_noncanonical_json_are_refused() {
        let encoded = encode(&Document::from_text("hello")).expect("encode");
        let json = std::str::from_utf8(&encoded[MAGIC.len() + 1..]).expect("json");
        let payload = |json: &str| {
            let mut bytes = MAGIC.to_vec();
            bytes.push(VERSION);
            bytes.extend_from_slice(json.as_bytes());
            bytes
        };
        let root_unknown = format!("{},\"futureField\":true}}", &json[..json.len() - 1]);
        assert!(matches!(decode(&payload(&root_unknown)), Err(BridgeError::UnsupportedRepresentation)));
        let needle = "\"text\":\"hello\"";
        assert!(json.contains(needle));
        let nested_unknown = json.replacen(needle, "\"text\":\"hello\",\"futureField\":true", 1);
        assert!(matches!(decode(&payload(&nested_unknown)), Err(BridgeError::UnsupportedRepresentation)));
        assert!(matches!(decode(&payload(&format!("{json} "))), Err(BridgeError::UnsupportedRepresentation)));
    }

    #[test]
    fn empty_container_is_a_blank_document() {
        assert!(decode(&[]).expect("blank").same_content(&Document::new()));
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
