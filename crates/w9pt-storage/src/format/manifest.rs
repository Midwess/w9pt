//! Canonical immutable file manifest and blob references.

use crate::{
    BLOCK_SIZE_V1, BlockSplitParameters, ContentRef, CorruptionError, Digest, FileId, FormatError,
    HashAlgorithm, LimitError, LimitKind, ObjectKey, PayloadCipher, PayloadCodec, Representation,
    StorageLimits, StorageMethod,
};

use super::{
    ObjectKind, PersistentFormatError, Reader, Writer, decode_envelope, encode_envelope,
    envelope::checked_envelope_len,
    head::{checked_key_length, read_key, write_key},
};

const METHOD_RAW: u8 = 1;
const METHOD_BLOCK_SPLIT: u8 = 2;
const HASH_BLAKE3_256: u8 = 1;
const CODEC_IDENTITY: u8 = 0;
const CIPHER_NONE: u8 = 0;

/// Reference to one immutable encoded raw-file or block payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlobRef {
    key: ObjectKey,
    plaintext_len: u64,
    stored_len: u64,
    digest: Digest,
    representation: Representation,
}

impl BlobRef {
    /// Creates a version-1 identity-encoded blob reference.
    pub fn new(key: ObjectKey, plaintext_len: u64, stored_len: u64, digest: Digest) -> Self {
        Self {
            key,
            plaintext_len,
            stored_len,
            digest,
            representation: Representation::V1,
        }
    }

    /// Returns the immutable payload key.
    pub fn key(&self) -> &ObjectKey {
        &self.key
    }

    /// Returns the canonical plaintext byte count.
    pub const fn plaintext_len(&self) -> u64 {
        self.plaintext_len
    }

    /// Returns the encoded stored byte count.
    pub const fn stored_len(&self) -> u64 {
        self.stored_len
    }

    /// Returns the canonical plaintext digest.
    pub const fn digest(&self) -> Digest {
        self.digest
    }

    /// Returns the persisted codec/cipher/hash representation.
    pub const fn representation(&self) -> Representation {
        self.representation
    }
}

/// One non-sparse block entry in ascending file-relative index order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockEntry {
    index: u64,
    blob: BlobRef,
}

impl BlockEntry {
    /// Creates a materialized block entry.
    pub fn new(index: u64, blob: BlobRef) -> Self {
        Self { index, blob }
    }

    /// Returns the file-relative logical block index.
    pub const fn index(&self) -> u64 {
        self.index
    }

    /// Returns the immutable canonical block reference.
    pub const fn blob(&self) -> &BlobRef {
        &self.blob
    }
}

/// Method-specific immutable manifest data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ManifestLayout {
    /// Whole-file layout, with no payload for an empty file.
    Raw {
        /// Complete canonical payload when logical size is nonzero.
        blob: Option<BlobRef>,
    },
    /// Fixed-size sparse logical block map.
    BlockSplit {
        /// Persisted format-version parameters.
        parameters: BlockSplitParameters,
        /// Sorted, unique materialized nonzero blocks.
        blocks: Vec<BlockEntry>,
    },
}

/// Immutable self-describing content manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileManifest {
    file_id: FileId,
    generation: u64,
    logical_size: u64,
    representation: Representation,
    layout: ManifestLayout,
}

impl FileManifest {
    /// Creates an empty or non-empty version-1 raw manifest.
    pub fn raw(file_id: FileId, generation: u64, logical_size: u64, blob: Option<BlobRef>) -> Self {
        Self {
            file_id,
            generation,
            logical_size,
            representation: Representation::V1,
            layout: ManifestLayout::Raw { blob },
        }
    }

    /// Creates a version-1 block-split manifest.
    pub fn block_split(
        file_id: FileId,
        generation: u64,
        logical_size: u64,
        blocks: Vec<BlockEntry>,
    ) -> Self {
        Self {
            file_id,
            generation,
            logical_size,
            representation: Representation::V1,
            layout: ManifestLayout::BlockSplit {
                parameters: BlockSplitParameters::V1,
                blocks,
            },
        }
    }

    /// Returns the owning file identity.
    pub const fn file_id(&self) -> FileId {
        self.file_id
    }

    /// Returns the immutable content generation.
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Returns authoritative logical EOF.
    pub const fn logical_size(&self) -> u64 {
        self.logical_size
    }

    /// Returns the persisted payload representation.
    pub const fn representation(&self) -> Representation {
        self.representation
    }

    /// Returns the persisted method-specific layout.
    pub const fn layout(&self) -> &ManifestLayout {
        &self.layout
    }

    /// Returns the explicitly persisted storage method.
    pub const fn method(&self) -> StorageMethod {
        match self.layout {
            ManifestLayout::Raw { .. } => StorageMethod::Raw,
            ManifestLayout::BlockSplit { .. } => StorageMethod::BlockSplit,
        }
    }

    /// Builds a portable reference to this manifest after immutable creation.
    pub(crate) fn content_ref(&self, key: ObjectKey, digest: Digest) -> ContentRef {
        ContentRef::new_validated(
            self.file_id,
            self.generation,
            self.logical_size,
            key,
            digest,
            self.method(),
        )
    }

    /// Checks every identity and summary field against a portable content reference.
    pub fn validate_content_ref(&self, content: &ContentRef) -> Result<(), CorruptionError> {
        if self.file_id != content.file_id() {
            return Err(CorruptionError::IdentityMismatch { field: "file" });
        }
        if self.generation != content.generation() {
            return Err(CorruptionError::IdentityMismatch {
                field: "generation",
            });
        }
        if self.logical_size != content.logical_size() {
            return Err(CorruptionError::IdentityMismatch {
                field: "logical size",
            });
        }
        if self.method() != content.method() {
            return Err(CorruptionError::IdentityMismatch {
                field: "storage method",
            });
        }
        Ok(())
    }
}

/// Encodes one canonical immutable manifest object.
pub fn encode_manifest(
    manifest: &FileManifest,
    limits: StorageLimits,
) -> Result<Vec<u8>, PersistentFormatError> {
    validate_manifest(manifest, limits)?;
    let payload_len = manifest_payload_len(manifest, limits)?;
    checked_envelope_len(
        payload_len,
        limits.max_manifest_bytes(),
        LimitKind::Manifest,
    )?;
    let mut writer = Writer::with_capacity(payload_len);
    writer.write_bytes(manifest.file_id.as_bytes());
    writer.write_u64(manifest.generation);
    writer.write_u64(manifest.logical_size);
    let method = match manifest.layout {
        ManifestLayout::Raw { .. } => METHOD_RAW,
        ManifestLayout::BlockSplit { .. } => METHOD_BLOCK_SPLIT,
    };
    writer.write_u8(method);
    write_representation(&mut writer, manifest.representation);
    match &manifest.layout {
        ManifestLayout::Raw { blob } => {
            writer.write_u8(u8::from(blob.is_some()));
            if let Some(blob) = blob {
                write_blob(&mut writer, blob, limits)?;
            }
        }
        ManifestLayout::BlockSplit { parameters, blocks } => {
            writer.write_u32(parameters.block_size());
            let count = u32::try_from(blocks.len()).map_err(|_| {
                LimitError::new(
                    LimitKind::BlockCount,
                    u64::MAX,
                    u64::from(limits.max_blocks()),
                )
            })?;
            writer.write_u32(count);
            for entry in blocks {
                writer.write_u64(entry.index);
                write_blob(&mut writer, &entry.blob, limits)?;
            }
        }
    }

    let payload = writer.into_bytes();
    debug_assert_eq!(payload.len(), payload_len);
    encode_envelope(ObjectKind::Manifest, &payload, limits.max_manifest_bytes()).map_err(|error| {
        LimitError::new(
            LimitKind::Manifest,
            error.actual,
            u64::try_from(limits.max_manifest_bytes()).unwrap_or(u64::MAX),
        )
        .into()
    })
}

fn manifest_payload_len(
    manifest: &FileManifest,
    limits: StorageLimits,
) -> Result<usize, PersistentFormatError> {
    let mut length =
        16_usize
            .checked_add(8 + 8 + 1 + 3)
            .ok_or(FormatError::ArithmeticOverflow {
                field: "manifest length",
            })?;
    match &manifest.layout {
        ManifestLayout::Raw { blob } => {
            length = checked_add_length(length, 1, "raw layout length")?;
            if let Some(blob) = blob {
                length = checked_add_length(length, blob_encoded_len(blob, limits)?, "raw blob")?;
            }
        }
        ManifestLayout::BlockSplit { blocks, .. } => {
            length = checked_add_length(length, 8, "block layout header")?;
            for entry in blocks {
                length = checked_add_length(length, 8, "block index")?;
                length = checked_add_length(
                    length,
                    blob_encoded_len(&entry.blob, limits)?,
                    "block blob",
                )?;
            }
        }
    }
    Ok(length)
}

fn blob_encoded_len(blob: &BlobRef, limits: StorageLimits) -> Result<usize, PersistentFormatError> {
    let key_len = usize::try_from(checked_key_length(&blob.key, limits)?).map_err(|_| {
        FormatError::ArithmeticOverflow {
            field: "object key length",
        }
    })?;
    4_usize
        .checked_add(key_len)
        .and_then(|length| length.checked_add(8 + 8 + 3 + 32))
        .ok_or_else(|| {
            FormatError::ArithmeticOverflow {
                field: "blob reference length",
            }
            .into()
        })
}

fn checked_add_length(
    current: usize,
    added: usize,
    field: &'static str,
) -> Result<usize, PersistentFormatError> {
    current
        .checked_add(added)
        .ok_or_else(|| FormatError::ArithmeticOverflow { field }.into())
}

/// Decodes one checked immutable manifest object.
pub fn decode_manifest(
    bytes: &[u8],
    limits: StorageLimits,
) -> Result<FileManifest, PersistentFormatError> {
    if bytes.len() > limits.max_manifest_bytes() {
        return Err(LimitError::new(
            LimitKind::Manifest,
            u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            u64::try_from(limits.max_manifest_bytes()).unwrap_or(u64::MAX),
        )
        .into());
    }
    let envelope = decode_envelope(ObjectKind::Manifest, bytes, limits.max_object_bytes())?;
    let mut reader = Reader::new(envelope.payload());
    let file_id = FileId::new(reader.read_array()?);
    let generation = reader.read_u64()?;
    let logical_size = reader.read_u64()?;
    let method = reader.read_u8()?;
    let representation = read_representation(&mut reader)?;
    let layout = match method {
        METHOD_RAW => {
            let blob = match reader.read_u8()? {
                0 => None,
                1 => Some(read_blob(&mut reader, limits)?),
                tag => {
                    return Err(FormatError::UnknownTag {
                        field: "raw blob presence",
                        tag: u64::from(tag),
                    }
                    .into());
                }
            };
            ManifestLayout::Raw { blob }
        }
        METHOD_BLOCK_SPLIT => {
            let block_size = reader.read_u32()?;
            let parameters = BlockSplitParameters::from_persisted(block_size).map_err(|_| {
                FormatError::UnknownTag {
                    field: "block size",
                    tag: u64::from(block_size),
                }
            })?;
            let count = reader.read_u32()?;
            if count > limits.max_blocks() {
                return Err(LimitError::new(
                    LimitKind::BlockCount,
                    u64::from(count),
                    u64::from(limits.max_blocks()),
                )
                .into());
            }
            const MINIMUM_BLOCK_ENTRY_BYTES: usize = 8 + 4 + 1 + 8 + 8 + 3 + 32;
            let count_usize =
                usize::try_from(count).map_err(|_| FormatError::ArithmeticOverflow {
                    field: "block entry count",
                })?;
            if count_usize > reader.remaining() / MINIMUM_BLOCK_ENTRY_BYTES {
                return Err(FormatError::InconsistentLength {
                    field: "block entries",
                }
                .into());
            }
            let mut blocks = Vec::with_capacity(count_usize);
            for _ in 0..count {
                blocks.push(BlockEntry::new(
                    reader.read_u64()?,
                    read_blob(&mut reader, limits)?,
                ));
            }
            ManifestLayout::BlockSplit { parameters, blocks }
        }
        tag => {
            return Err(FormatError::UnknownTag {
                field: "storage method",
                tag: u64::from(tag),
            }
            .into());
        }
    };
    reader.finish()?;
    let manifest = FileManifest {
        file_id,
        generation,
        logical_size,
        representation,
        layout,
    };
    validate_manifest(&manifest, limits)?;
    Ok(manifest)
}

fn validate_manifest(
    manifest: &FileManifest,
    limits: StorageLimits,
) -> Result<(), PersistentFormatError> {
    if manifest.generation == 0 {
        return Err(FormatError::NonCanonical {
            field: "zero generation",
        }
        .into());
    }
    if manifest.representation != Representation::V1 {
        return Err(FormatError::NonCanonical {
            field: "manifest representation",
        }
        .into());
    }

    match &manifest.layout {
        ManifestLayout::Raw { blob } => {
            if manifest.logical_size > limits.max_raw_file_bytes() {
                return Err(LimitError::new(
                    LimitKind::RawFile,
                    manifest.logical_size,
                    limits.max_raw_file_bytes(),
                )
                .into());
            }
            match (manifest.logical_size, blob) {
                (0, None) => {}
                (0, Some(_)) | (_, None) => {
                    return Err(FormatError::NonCanonical {
                        field: "raw empty payload",
                    }
                    .into());
                }
                (logical_size, Some(blob)) => {
                    validate_blob(blob, limits)?;
                    if blob.plaintext_len != logical_size {
                        return Err(FormatError::InconsistentLength {
                            field: "raw plaintext",
                        }
                        .into());
                    }
                }
            }
        }
        ManifestLayout::BlockSplit { parameters, blocks } => {
            if *parameters != BlockSplitParameters::V1 {
                return Err(FormatError::NonCanonical {
                    field: "block parameters",
                }
                .into());
            }
            let actual = u64::try_from(blocks.len()).unwrap_or(u64::MAX);
            if actual > u64::from(limits.max_blocks()) {
                return Err(LimitError::new(
                    LimitKind::BlockCount,
                    actual,
                    u64::from(limits.max_blocks()),
                )
                .into());
            }
            let zero_digest = Digest::blake3(&[0; BLOCK_SIZE_V1 as usize]);
            let mut previous = None;
            for entry in blocks {
                if previous.is_some_and(|index| entry.index <= index) {
                    return Err(FormatError::NonCanonical {
                        field: "block ordering",
                    }
                    .into());
                }
                previous = Some(entry.index);
                let block_start = entry.index.checked_mul(u64::from(BLOCK_SIZE_V1)).ok_or(
                    FormatError::ArithmeticOverflow {
                        field: "block offset",
                    },
                )?;
                if block_start >= manifest.logical_size {
                    return Err(FormatError::NonCanonical {
                        field: "block beyond logical EOF",
                    }
                    .into());
                }
                validate_blob(&entry.blob, limits)?;
                if entry.blob.plaintext_len != u64::from(BLOCK_SIZE_V1) {
                    return Err(FormatError::InconsistentLength {
                        field: "block plaintext",
                    }
                    .into());
                }
                if entry.blob.digest == zero_digest {
                    return Err(FormatError::NonCanonical {
                        field: "materialized zero block",
                    }
                    .into());
                }
            }
        }
    }
    Ok(())
}

fn validate_blob(blob: &BlobRef, limits: StorageLimits) -> Result<(), PersistentFormatError> {
    checked_key_length(&blob.key, limits)?;
    if blob.representation != Representation::V1 {
        return Err(FormatError::NonCanonical {
            field: "blob representation",
        }
        .into());
    }
    if blob.plaintext_len == 0 || blob.stored_len != blob.plaintext_len {
        return Err(FormatError::InconsistentLength {
            field: "identity payload",
        }
        .into());
    }
    if blob.stored_len > u64::try_from(limits.max_object_bytes()).unwrap_or(u64::MAX) {
        return Err(LimitError::new(
            LimitKind::Object,
            blob.stored_len,
            u64::try_from(limits.max_object_bytes()).unwrap_or(u64::MAX),
        )
        .into());
    }
    Ok(())
}

fn write_blob(
    writer: &mut Writer,
    blob: &BlobRef,
    limits: StorageLimits,
) -> Result<(), PersistentFormatError> {
    write_key(writer, &blob.key, limits)?;
    writer.write_u64(blob.plaintext_len);
    writer.write_u64(blob.stored_len);
    write_representation(writer, blob.representation);
    writer.write_bytes(blob.digest.as_bytes());
    Ok(())
}

fn read_blob(
    reader: &mut Reader<'_>,
    limits: StorageLimits,
) -> Result<BlobRef, PersistentFormatError> {
    let key = read_key(reader, limits)?;
    let plaintext_len = reader.read_u64()?;
    let stored_len = reader.read_u64()?;
    let representation = read_representation(reader)?;
    let digest = Digest::new(reader.read_array()?);
    Ok(BlobRef {
        key,
        plaintext_len,
        stored_len,
        digest,
        representation,
    })
}

fn write_representation(writer: &mut Writer, representation: Representation) {
    let hash = match representation.hash() {
        HashAlgorithm::Blake3_256 => HASH_BLAKE3_256,
    };
    let codec = match representation.codec() {
        PayloadCodec::Identity => CODEC_IDENTITY,
    };
    let cipher = match representation.cipher() {
        PayloadCipher::None => CIPHER_NONE,
    };
    writer.write_u8(hash);
    writer.write_u8(codec);
    writer.write_u8(cipher);
}

fn read_representation(reader: &mut Reader<'_>) -> Result<Representation, PersistentFormatError> {
    let hash = reader.read_u8()?;
    if hash != HASH_BLAKE3_256 {
        return Err(FormatError::UnknownTag {
            field: "hash algorithm",
            tag: u64::from(hash),
        }
        .into());
    }
    let codec = reader.read_u8()?;
    if codec != CODEC_IDENTITY {
        return Err(FormatError::UnknownTag {
            field: "payload codec",
            tag: u64::from(codec),
        }
        .into());
    }
    let cipher = reader.read_u8()?;
    if cipher != CIPHER_NONE {
        return Err(FormatError::UnknownTag {
            field: "payload cipher",
            tag: u64::from(cipher),
        }
        .into());
    }
    Ok(Representation::V1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(name: &str, length: u64, digest_byte: u8) -> BlobRef {
        BlobRef::new(
            ObjectKey::new(name).unwrap(),
            length,
            length,
            Digest::new([digest_byte; 32]),
        )
    }

    #[test]
    fn raw_manifest_round_trips() {
        let manifest =
            FileManifest::raw(FileId::from_u128(1), 2, 3, Some(blob("v1/data/raw", 3, 4)));
        let bytes = encode_manifest(&manifest, StorageLimits::default()).unwrap();
        assert_eq!(
            decode_manifest(&bytes, StorageLimits::default()),
            Ok(manifest)
        );
    }

    #[test]
    fn block_manifest_round_trips() {
        let manifest = FileManifest::block_split(
            FileId::from_u128(1),
            2,
            65_536,
            vec![BlockEntry::new(1, blob("v1/data/block/1", 32_768, 5))],
        );
        let bytes = encode_manifest(&manifest, StorageLimits::default()).unwrap();
        assert_eq!(
            decode_manifest(&bytes, StorageLimits::default()),
            Ok(manifest)
        );
    }

    #[test]
    fn invalid_raw_and_block_relationships_are_rejected() {
        let invalid_raw = FileManifest::raw(FileId::from_u128(1), 1, 2, None);
        assert!(matches!(
            encode_manifest(&invalid_raw, StorageLimits::default()),
            Err(PersistentFormatError::Format(
                FormatError::NonCanonical { .. }
            ))
        ));

        let invalid_blocks = FileManifest::block_split(
            FileId::from_u128(1),
            1,
            65_536,
            vec![
                BlockEntry::new(1, blob("v1/data/block/1", 32_768, 5)),
                BlockEntry::new(1, blob("v1/data/block/duplicate", 32_768, 6)),
            ],
        );
        assert!(matches!(
            encode_manifest(&invalid_blocks, StorageLimits::default()),
            Err(PersistentFormatError::Format(
                FormatError::NonCanonical { .. }
            ))
        ));
    }

    #[test]
    fn manifest_context_validation_detects_mismatched_content_ref() {
        let manifest = FileManifest::raw(FileId::from_u128(1), 1, 0, None);
        let content = ContentRef::from_persisted(
            FileId::from_u128(2),
            1,
            0,
            ObjectKey::new("manifest").unwrap(),
            Digest::new([0; 32]),
            StorageMethod::Raw,
        )
        .unwrap();
        assert_eq!(
            manifest.validate_content_ref(&content),
            Err(CorruptionError::IdentityMismatch { field: "file" })
        );
    }

    #[test]
    fn manifest_encoder_rejects_oversized_blob_key() {
        let limits = StorageLimits::default();
        let manifest = FileManifest::raw(
            FileId::from_u128(1),
            1,
            1,
            Some(BlobRef::new(
                ObjectKey::new("x".repeat(limits.max_key_bytes() + 1)).unwrap(),
                1,
                1,
                Digest::new([1; 32]),
            )),
        );
        assert!(matches!(
            encode_manifest(&manifest, limits),
            Err(PersistentFormatError::Limit(LimitError {
                kind: LimitKind::Key,
                ..
            }))
        ));
    }

    #[test]
    fn precomputed_manifest_payload_length_matches_encoded_envelope() {
        let limits = StorageLimits::default();
        let manifest = FileManifest::block_split(
            FileId::from_u128(1),
            2,
            65_536,
            vec![BlockEntry::new(1, blob("v1/data/block/1", 32_768, 5))],
        );
        let expected = manifest_payload_len(&manifest, limits).unwrap();
        let encoded = encode_manifest(&manifest, limits).unwrap();
        let envelope = decode_envelope(ObjectKind::Manifest, &encoded, encoded.len()).unwrap();
        assert_eq!(envelope.payload().len(), expected);
    }
}
