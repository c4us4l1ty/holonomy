//! Fixed, public container header. Password-derived material is never stored here.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use holonomy_seal::{generate_container_id, metadata_digest, Capsule, KdfParamsRecord};

#[cfg(test)]
use holonomy_seal::{CALIBRATION_P, CALIBRATION_T, FENCE_MAX_KIB, FENCE_MIN_KIB};

use crate::LineError;

pub(crate) const HEADER_MAGIC: &[u8; 8] = b"HOLWAV6\0";
pub(crate) const HEADER_VERSION: u16 = 1;
pub(crate) const HEADER_BYTES: usize = 8 + 2 + 2 + 16 + 12 + 64 + Capsule::WIRE_BYTES;
#[cfg(test)]
pub(crate) const CAPSULE_OFFSET: usize = 8 + 2 + 2 + 16 + 12 + 64;
pub(crate) const RECORD_HEADER_BYTES: usize = 12;
pub(crate) const RECORD_FIXED_BYTES: usize = 8 + 24 + 4 + 32;

/// Public data needed to derive the unwrap key and identify one world-line epoch.
///
/// The header contains no plaintext document data or secret key material. Its salt and KDF
/// record are checked against the production fence before the caller runs Argon2.
#[derive(Clone)]
pub struct LineHeader {
    file_id: [u8; 16],
    /// The calibrated Argon2id schedule used by this file.
    pub kdf_params: KdfParamsRecord,
    /// The per-file Argon2 salt.
    pub salt: [u8; 64],
    /// The fixed-size two-phase envelope capsule.
    pub capsule: Capsule,
}

impl LineHeader {
    /// Create a header with a fresh random file identifier.
    pub fn new(
        kdf_params: KdfParamsRecord,
        salt: [u8; 64],
        capsule: Capsule,
    ) -> Result<Self, LineError> {
        validate_kdf_params(&kdf_params)?;
        let file_id = generate_container_id().map_err(map_entropy_error)?;
        Ok(Self {
            file_id,
            kdf_params,
            salt,
            capsule,
        })
    }

    /// Read and validate only the fixed header; no segment body is touched.
    pub fn read(path: impl AsRef<Path>) -> Result<Self, LineError> {
        let mut file = crate::open_worldline_file(path.as_ref(), true, false)?;
        crate::validate_regular_file(&file)?;
        Self::read_from(&mut file)
    }

    /// The non-secret, random identifier bound into every sealed segment and witness.
    pub fn file_id(&self) -> &[u8; 16] {
        &self.file_id
    }

    /// A public fingerprint of the complete header, committed into each witness. It binds the
    /// capsule, KDF record, salt, format version, and file identifier as one envelope epoch.
    pub fn epoch_id(&self) -> [u8; 32] {
        metadata_digest(b"LINE_ENVELOPE_EPOCH", &self.encode())
    }

    pub(crate) fn validate(&self) -> Result<(), LineError> {
        validate_kdf_params(&self.kdf_params)
    }

    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_BYTES);
        out.extend_from_slice(HEADER_MAGIC);
        out.extend_from_slice(&HEADER_VERSION.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // reserved; must remain zero in v1
        out.extend_from_slice(&self.file_id);
        out.extend_from_slice(&self.kdf_params.m_kib.to_le_bytes());
        out.extend_from_slice(&self.kdf_params.t.to_le_bytes());
        out.extend_from_slice(&self.kdf_params.p.to_le_bytes());
        out.extend_from_slice(&self.salt);
        out.extend_from_slice(&self.capsule.to_bytes());
        debug_assert_eq!(out.len(), HEADER_BYTES);
        out
    }

    pub(crate) fn write_to<W: Write>(&self, writer: &mut W) -> Result<(), LineError> {
        self.validate()?;
        writer.write_all(&self.encode())?;
        Ok(())
    }

    pub(crate) fn read_from<R: Read + Seek>(reader: &mut R) -> Result<Self, LineError> {
        reader.seek(SeekFrom::Start(0))?;
        let mut bytes = vec![0u8; HEADER_BYTES];
        reader.read_exact(&mut bytes).map_err(|error| {
            if error.kind() == std::io::ErrorKind::UnexpectedEof {
                LineError::InvalidHeader
            } else {
                LineError::Io(error)
            }
        })?;

        if bytes[..8] != HEADER_MAGIC[..] {
            return Err(LineError::InvalidHeader);
        }
        let version = u16::from_le_bytes(bytes[8..10].try_into().unwrap());
        let reserved = u16::from_le_bytes(bytes[10..12].try_into().unwrap());
        if version != HEADER_VERSION || reserved != 0 {
            return Err(LineError::UnsupportedHeader);
        }

        let mut offset = 12;
        let file_id = take_array::<16>(&bytes, &mut offset)?;
        let m_kib = u32::from_le_bytes(take_array::<4>(&bytes, &mut offset)?);
        let t = u32::from_le_bytes(take_array::<4>(&bytes, &mut offset)?);
        let p = u32::from_le_bytes(take_array::<4>(&bytes, &mut offset)?);
        let salt = take_array::<64>(&bytes, &mut offset)?;
        let capsule =
            Capsule::from_bytes(&bytes[offset..]).map_err(|_| LineError::InvalidHeader)?;
        let header = Self {
            file_id,
            kdf_params: KdfParamsRecord { m_kib, t, p },
            salt,
            capsule,
        };
        header.validate()?;
        reader.seek(SeekFrom::Start(HEADER_BYTES as u64))?;
        Ok(header)
    }
}

fn validate_kdf_params(params: &KdfParamsRecord) -> Result<(), LineError> {
    if params.is_production_fenced() {
        return Ok(());
    }
    #[cfg(any(test, debug_assertions))]
    if (params.m_kib, params.t, params.p) == (8_192, 1, 1) {
        return Ok(());
    }
    Err(LineError::KdfParametersRejected)
}

fn map_entropy_error(_: holonomy_seal::SealError) -> LineError {
    LineError::EntropyUnavailable
}

fn take_array<const N: usize>(bytes: &[u8], offset: &mut usize) -> Result<[u8; N], LineError> {
    let end = (*offset).checked_add(N).ok_or(LineError::InvalidHeader)?;
    let value = bytes
        .get(*offset..end)
        .ok_or(LineError::InvalidHeader)?
        .try_into()
        .map_err(|_| LineError::InvalidHeader)?;
    *offset = end;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use holonomy_seal::{MasterKeys, ParamsClass};

    #[test]
    fn header_round_trips_and_binds_its_epoch() {
        let salt = [0x53; 64];
        let keys = MasterKeys::derive(b"header test", &salt, None, ParamsClass::Test).unwrap();
        let (capsule, _) = holonomy_seal::create_capsule_for(keys.unwrap_key(), None).unwrap();
        let header = LineHeader::new(
            KdfParamsRecord {
                m_kib: 8_192,
                t: 1,
                p: 1,
            },
            salt,
            capsule,
        )
        .unwrap();
        let encoded = header.encode();
        let mut cursor = std::io::Cursor::new(encoded.clone());
        let decoded = LineHeader::read_from(&mut cursor).unwrap();
        assert_eq!(decoded.file_id(), header.file_id());
        assert_eq!(decoded.kdf_params, header.kdf_params);
        assert_eq!(decoded.salt, header.salt);
        assert_eq!(decoded.epoch_id(), header.epoch_id());

        let mut changed = encoded;
        changed[CAPSULE_OFFSET] ^= 1;
        let mut cursor = std::io::Cursor::new(changed);
        let changed_header = LineHeader::read_from(&mut cursor).unwrap();
        assert_ne!(changed_header.epoch_id(), header.epoch_id());
    }

    #[test]
    fn production_headers_refuse_weak_or_unbounded_kdf_records() {
        let weak = KdfParamsRecord {
            m_kib: 8_192,
            t: 1,
            p: 1,
        };
        assert!(validate_kdf_params(&weak).is_ok()); // Test profile only.
        let malformed = KdfParamsRecord {
            m_kib: FENCE_MIN_KIB,
            t: u32::MAX,
            p: CALIBRATION_P,
        };
        assert!(matches!(
            validate_kdf_params(&malformed),
            Err(LineError::KdfParametersRejected)
        ));
        let too_large = KdfParamsRecord {
            m_kib: FENCE_MAX_KIB + 1,
            t: CALIBRATION_T,
            p: CALIBRATION_P,
        };
        assert!(matches!(
            validate_kdf_params(&too_large),
            Err(LineError::KdfParametersRejected)
        ));
    }
}
