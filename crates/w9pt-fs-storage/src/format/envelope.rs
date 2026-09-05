//! Checked outer envelope shared by heads, manifests, and data payloads.

use crate::{CorruptionError, Digest, FormatError, LimitError, LimitKind};

use super::{PersistentFormatError, Reader, Writer};

const MAGIC: [u8; 8] = *b"W9PTOBJ\0";
const FORMAT_MAJOR: u16 = 1;
const FORMAT_MINOR: u16 = 0;
pub(crate) const HEADER_LEN: usize = 8 + 1 + 2 + 2 + 8 + 32;

/// Kind of checked persistent object.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ObjectKind {
    /// Mutable file publication head.
    Head = 1,
    /// Immutable file-content manifest.
    Manifest = 2,
    /// Immutable canonical identity payload.
    Payload = 3,
}

/// Validated borrowed envelope payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Envelope<'a> {
    kind: ObjectKind,
    payload: &'a [u8],
}

impl<'a> Envelope<'a> {
    /// Returns the validated object kind.
    pub const fn kind(self) -> ObjectKind {
        self.kind
    }

    /// Returns the checksum-verified payload bytes.
    pub const fn payload(self) -> &'a [u8] {
        self.payload
    }
}

/// Encodes a version-1 checked object, enforcing the complete object bound.
pub fn encode_envelope(
    kind: ObjectKind,
    payload: &[u8],
    max_object_bytes: usize,
) -> Result<Vec<u8>, LimitError> {
    let total = checked_envelope_len(payload.len(), max_object_bytes, LimitKind::Object)?;
    let payload_len = u64::try_from(payload.len()).map_err(|_| {
        LimitError::new(LimitKind::Object, u64::MAX, usize_to_u64(max_object_bytes))
    })?;
    let checksum = Digest::blake3(payload);
    let mut writer = Writer::with_capacity(total);
    writer.write_bytes(&MAGIC);
    writer.write_u8(kind as u8);
    writer.write_u16(FORMAT_MAJOR);
    writer.write_u16(FORMAT_MINOR);
    writer.write_u64(payload_len);
    writer.write_bytes(checksum.as_bytes());
    writer.write_bytes(payload);
    Ok(writer.into_bytes())
}

pub(crate) fn checked_envelope_len(
    payload_len: usize,
    max_object_bytes: usize,
    kind: LimitKind,
) -> Result<usize, LimitError> {
    let total = HEADER_LEN
        .checked_add(payload_len)
        .ok_or_else(|| LimitError::new(kind, u64::MAX, usize_to_u64(max_object_bytes)))?;
    if total > max_object_bytes {
        Err(LimitError::new(
            kind,
            usize_to_u64(total),
            usize_to_u64(max_object_bytes),
        ))
    } else {
        Ok(total)
    }
}

/// Decodes and verifies one version-1 checked object without allocating payload bytes.
pub fn decode_envelope(
    expected_kind: ObjectKind,
    bytes: &[u8],
    max_object_bytes: usize,
) -> Result<Envelope<'_>, PersistentFormatError> {
    if bytes.len() > max_object_bytes {
        return Err(LimitError::new(
            LimitKind::Object,
            usize_to_u64(bytes.len()),
            usize_to_u64(max_object_bytes),
        )
        .into());
    }

    let mut reader = Reader::new(bytes);
    if reader.read_array::<8>()? != MAGIC {
        return Err(FormatError::InvalidMagic.into());
    }
    let actual_kind = reader.read_u8()?;
    if actual_kind != expected_kind as u8 {
        return Err(FormatError::UnexpectedKind {
            expected: expected_kind as u8,
            actual: actual_kind,
        }
        .into());
    }
    let major = reader.read_u16()?;
    let minor = reader.read_u16()?;
    if major != FORMAT_MAJOR || minor != FORMAT_MINOR {
        return Err(FormatError::UnsupportedVersion { major, minor }.into());
    }
    let payload_len = reader.read_u64()?;
    let payload_len =
        usize::try_from(payload_len).map_err(|_| FormatError::ArithmeticOverflow {
            field: "envelope payload length",
        })?;
    let checksum = Digest::new(reader.read_array()?);
    if reader.remaining() != payload_len {
        return Err(FormatError::InconsistentLength {
            field: "envelope payload",
        }
        .into());
    }
    let payload = reader.read_bytes(payload_len)?;
    reader.finish()?;
    if Digest::blake3(payload) != checksum {
        return Err(CorruptionError::ChecksumMismatch.into());
    }
    Ok(Envelope {
        kind: expected_kind,
        payload,
    })
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_round_trips_and_binds_kind() {
        let bytes = encode_envelope(ObjectKind::Manifest, b"manifest", 1024).unwrap();
        let decoded = decode_envelope(ObjectKind::Manifest, &bytes, 1024).unwrap();
        assert_eq!(decoded.kind(), ObjectKind::Manifest);
        assert_eq!(decoded.payload(), b"manifest");
        assert!(matches!(
            decode_envelope(ObjectKind::Head, &bytes, 1024),
            Err(PersistentFormatError::Format(
                FormatError::UnexpectedKind { .. }
            ))
        ));
    }

    #[test]
    fn envelope_detects_checksum_corruption_and_bounds() {
        let mut bytes = encode_envelope(ObjectKind::Payload, b"payload", 1024).unwrap();
        *bytes.last_mut().unwrap() ^= 0xff;
        assert_eq!(
            decode_envelope(ObjectKind::Payload, &bytes, 1024),
            Err(PersistentFormatError::Corruption(
                CorruptionError::ChecksumMismatch
            ))
        );
        assert!(encode_envelope(ObjectKind::Payload, b"payload", 4).is_err());
        assert!(matches!(
            decode_envelope(ObjectKind::Payload, &bytes, 4),
            Err(PersistentFormatError::Limit(_))
        ));
    }
}
