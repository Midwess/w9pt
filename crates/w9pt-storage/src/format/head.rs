//! Canonical mutable file-head payload.

use crate::{
    CorruptionError, Digest, FileId, FormatError, LimitError, LimitKind, MutationId, ObjectKey,
    StorageLimits,
};

use super::{ObjectKind, PersistentFormatError, Reader, Writer, decode_envelope, encode_envelope};

/// Mutable publication record selecting one immutable file manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileHead {
    file_id: FileId,
    generation: u64,
    manifest_key: ObjectKey,
    manifest_digest: Digest,
    mutation_id: MutationId,
}

impl FileHead {
    /// Creates a head from a fully prepared immutable manifest.
    pub fn new(
        file_id: FileId,
        generation: u64,
        manifest_key: ObjectKey,
        manifest_digest: Digest,
        mutation_id: MutationId,
    ) -> Self {
        Self {
            file_id,
            generation,
            manifest_key,
            manifest_digest,
            mutation_id,
        }
    }

    /// Returns the owning file identity.
    pub const fn file_id(&self) -> FileId {
        self.file_id
    }

    /// Returns the published generation.
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Returns the selected immutable manifest key.
    pub fn manifest_key(&self) -> &ObjectKey {
        &self.manifest_key
    }

    /// Returns the expected digest of the complete encoded manifest.
    pub const fn manifest_digest(&self) -> Digest {
        self.manifest_digest
    }

    /// Returns the stable identity of the mutation that prepared this head.
    pub const fn mutation_id(&self) -> MutationId {
        self.mutation_id
    }

    /// Checks that this head was loaded for the requested opaque file.
    pub fn validate_file(&self, expected: FileId) -> Result<(), CorruptionError> {
        if self.file_id == expected {
            Ok(())
        } else {
            Err(CorruptionError::IdentityMismatch { field: "file" })
        }
    }
}

/// Encodes one canonical file-head object.
pub fn encode_head(
    head: &FileHead,
    limits: StorageLimits,
) -> Result<Vec<u8>, PersistentFormatError> {
    validate_head(head, limits)?;
    let mut writer = Writer::new();
    writer.write_bytes(head.file_id.as_bytes());
    writer.write_u64(head.generation);
    write_key(&mut writer, &head.manifest_key, limits)?;
    writer.write_bytes(head.manifest_digest.as_bytes());
    writer.write_bytes(head.mutation_id.as_bytes());
    encode_envelope(
        ObjectKind::Head,
        &writer.into_bytes(),
        limits.max_object_bytes(),
    )
    .map_err(Into::into)
}

/// Decodes one checked canonical file-head object.
pub fn decode_head(bytes: &[u8], limits: StorageLimits) -> Result<FileHead, PersistentFormatError> {
    let envelope = decode_envelope(ObjectKind::Head, bytes, limits.max_object_bytes())?;
    let mut reader = Reader::new(envelope.payload());
    let file_id = FileId::new(reader.read_array()?);
    let generation = reader.read_u64()?;
    let manifest_key = read_key(&mut reader, limits)?;
    let manifest_digest = Digest::new(reader.read_array()?);
    let mutation_id = MutationId::new(reader.read_array()?);
    reader.finish()?;
    let head = FileHead::new(
        file_id,
        generation,
        manifest_key,
        manifest_digest,
        mutation_id,
    );
    validate_head(&head, limits)?;
    Ok(head)
}

fn validate_head(head: &FileHead, limits: StorageLimits) -> Result<(), PersistentFormatError> {
    if head.generation == 0 {
        return Err(FormatError::NonCanonical {
            field: "zero generation",
        }
        .into());
    }
    checked_key_length(&head.manifest_key, limits)?;
    Ok(())
}

pub(super) fn write_key(
    writer: &mut Writer,
    key: &ObjectKey,
    limits: StorageLimits,
) -> Result<(), PersistentFormatError> {
    let length = checked_key_length(key, limits)?;
    writer.write_u32(length);
    writer.write_bytes(key.as_str().as_bytes());
    Ok(())
}

pub(super) fn checked_key_length(
    key: &ObjectKey,
    limits: StorageLimits,
) -> Result<u32, PersistentFormatError> {
    let actual = key.as_str().len();
    if actual > limits.max_key_bytes() {
        return Err(LimitError::new(
            LimitKind::Key,
            u64::try_from(actual).unwrap_or(u64::MAX),
            u64::try_from(limits.max_key_bytes()).unwrap_or(u64::MAX),
        )
        .into());
    }
    u32::try_from(actual).map_err(|_| {
        FormatError::ArithmeticOverflow {
            field: "object key length",
        }
        .into()
    })
}

pub(super) fn read_key(
    reader: &mut Reader<'_>,
    limits: StorageLimits,
) -> Result<ObjectKey, PersistentFormatError> {
    let length = reader.read_u32()?;
    let length = usize::try_from(length).map_err(|_| FormatError::ArithmeticOverflow {
        field: "object key length",
    })?;
    if length > limits.max_key_bytes() {
        return Err(crate::LimitError::new(
            crate::LimitKind::Key,
            u64::try_from(length).unwrap_or(u64::MAX),
            u64::try_from(limits.max_key_bytes()).unwrap_or(u64::MAX),
        )
        .into());
    }
    let value = core::str::from_utf8(reader.read_bytes(length)?).map_err(|_| {
        FormatError::NonCanonical {
            field: "object key",
        }
    })?;
    ObjectKey::new(value).map_err(|_| {
        FormatError::NonCanonical {
            field: "object key",
        }
        .into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_round_trips() {
        let head = FileHead::new(
            FileId::from_u128(1),
            7,
            ObjectKey::new("private/v1/manifest").unwrap(),
            Digest::new([3; 32]),
            MutationId::from_u128(2),
        );
        let bytes = encode_head(&head, StorageLimits::default()).unwrap();
        assert_eq!(decode_head(&bytes, StorageLimits::default()), Ok(head));
    }

    #[test]
    fn head_encoder_rejects_key_its_decoder_would_reject() {
        let limits = StorageLimits::default();
        let head = FileHead::new(
            FileId::from_u128(1),
            1,
            ObjectKey::new("x".repeat(limits.max_key_bytes() + 1)).unwrap(),
            Digest::new([3; 32]),
            MutationId::from_u128(2),
        );
        assert!(matches!(
            encode_head(&head, limits),
            Err(PersistentFormatError::Limit(LimitError {
                kind: LimitKind::Key,
                ..
            }))
        ));
    }
}
