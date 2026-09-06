//! Canonical immutable compact file manifests and payload references.

use crate::{
    BlockSplitParameters, ContentRef, CorruptionError, Digest, FileId, FormatError, HashAlgorithm,
    LimitError, LimitKind, ObjectKey, PayloadCipher, PayloadCodec, Representation, StorageLimits,
    StorageMethod,
};

use super::{
    ObjectKind, PageRef, PersistentFormatError, Reader, Writer,
    block_map::{
        minimum_root_level, page_ref_encoded_len, read_page_ref, read_profile, validate_page_ref,
        write_page_ref, write_profile,
    },
    decode_envelope, encode_envelope,
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
    /// Creates an identity-encoded blob reference for the current format.
    pub fn new(key: ObjectKey, plaintext_len: u64, stored_len: u64, digest: Digest) -> Self {
        Self {
            key,
            plaintext_len,
            stored_len,
            digest,
            representation: Representation::CURRENT,
        }
    }

    /// Returns the immutable payload key.
    pub const fn key(&self) -> &ObjectKey {
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

/// Method-specific immutable manifest data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ManifestLayout {
    /// Whole-file layout, with no payload for an empty file.
    Raw {
        /// Complete canonical payload when logical size is nonzero.
        blob: Option<BlobRef>,
    },
    /// Fixed-size sparse logical block map selected by one optional root.
    BlockSplit {
        /// Persisted radix-tree parameters.
        parameters: BlockSplitParameters,
        /// Authenticated sparse map root; absent for an all-hole file.
        root: Option<PageRef>,
    },
}

/// Immutable self-describing compact content manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileManifest {
    file_id: FileId,
    generation: u64,
    logical_size: u64,
    representation: Representation,
    layout: ManifestLayout,
}

impl FileManifest {
    /// Creates an empty or non-empty current raw manifest.
    pub fn raw(file_id: FileId, generation: u64, logical_size: u64, blob: Option<BlobRef>) -> Self {
        Self {
            file_id,
            generation,
            logical_size,
            representation: Representation::CURRENT,
            layout: ManifestLayout::Raw { blob },
        }
    }

    /// Creates a current paged block-split manifest.
    pub fn block_split(
        file_id: FileId,
        generation: u64,
        logical_size: u64,
        root: Option<PageRef>,
    ) -> Self {
        Self {
            file_id,
            generation,
            logical_size,
            representation: Representation::CURRENT,
            layout: ManifestLayout::BlockSplit {
                parameters: BlockSplitParameters::CURRENT,
                root,
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

    /// Checks identity and summary fields against a portable content reference.
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

/// Encodes one canonical immutable compact manifest object.
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
    writer.write_u8(match manifest.layout {
        ManifestLayout::Raw { .. } => METHOD_RAW,
        ManifestLayout::BlockSplit { .. } => METHOD_BLOCK_SPLIT,
    });
    write_representation(&mut writer, manifest.representation);
    match &manifest.layout {
        ManifestLayout::Raw { blob } => {
            writer.write_u8(u8::from(blob.is_some()));
            if let Some(blob) = blob {
                write_blob(&mut writer, blob, limits)?;
            }
        }
        ManifestLayout::BlockSplit { parameters, root } => {
            write_profile(&mut writer, *parameters);
            writer.write_u8(u8::from(root.is_some()));
            if let Some(root) = root {
                write_page_ref(&mut writer, root, limits)?;
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

/// Decodes one checked immutable compact manifest object.
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
    let envelope = decode_envelope(ObjectKind::Manifest, bytes, limits.max_manifest_bytes())?;
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
            let parameters = read_profile(&mut reader)?;
            let root = match reader.read_u8()? {
                0 => None,
                1 => Some(read_page_ref(&mut reader, limits)?),
                tag => {
                    return Err(FormatError::UnknownTag {
                        field: "map root presence",
                        tag: u64::from(tag),
                    }
                    .into());
                }
            };
            ManifestLayout::BlockSplit { parameters, root }
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
        ManifestLayout::BlockSplit { root, .. } => {
            length = checked_add_length(length, 8 + 1, "block layout header")?;
            if let Some(root) = root {
                length =
                    checked_add_length(length, page_ref_encoded_len(root, limits)?, "map root")?;
            }
        }
    }
    Ok(length)
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
    if manifest.representation != Representation::CURRENT {
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
        ManifestLayout::BlockSplit { parameters, root } => {
            if *parameters != BlockSplitParameters::CURRENT {
                return Err(FormatError::NonCanonical {
                    field: "block parameters",
                }
                .into());
            }
            if let Some(root) = root {
                validate_page_ref(root, limits)?;
                if root.first_block() != 0 {
                    return Err(FormatError::NonCanonical {
                        field: "map root start",
                    }
                    .into());
                }
                if root.level() != minimum_root_level(root.highest_materialized_block())? {
                    return Err(FormatError::NonCanonical {
                        field: "map root normalization",
                    }
                    .into());
                }
                if root.materialized_block_count() > limits.max_materialized_blocks() {
                    return Err(LimitError::new(
                        LimitKind::MaterializedBlocks,
                        root.materialized_block_count(),
                        limits.max_materialized_blocks(),
                    )
                    .into());
                }
                let highest_start = root
                    .highest_materialized_block()
                    .checked_mul(u64::from(crate::BLOCK_SIZE))
                    .ok_or(FormatError::ArithmeticOverflow {
                        field: "highest block offset",
                    })?;
                if highest_start >= manifest.logical_size {
                    return Err(FormatError::NonCanonical {
                        field: "map root beyond logical EOF",
                    }
                    .into());
                }
            }
        }
    }
    Ok(())
}

pub(super) fn blob_encoded_len(
    blob: &BlobRef,
    limits: StorageLimits,
) -> Result<usize, PersistentFormatError> {
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

pub(super) fn validate_blob(
    blob: &BlobRef,
    limits: StorageLimits,
) -> Result<(), PersistentFormatError> {
    checked_key_length(&blob.key, limits)?;
    if blob.representation != Representation::CURRENT {
        return Err(FormatError::NonCanonical {
            field: "blob representation",
        }
        .into());
    }
    if blob.plaintext_len == 0 || blob.stored_len < crate::representation::PAYLOAD_MIN_STORED_BYTES
    {
        return Err(FormatError::InconsistentLength {
            field: "stored payload",
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

pub(super) fn write_blob(
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

pub(super) fn read_blob(
    reader: &mut Reader<'_>,
    limits: StorageLimits,
) -> Result<BlobRef, PersistentFormatError> {
    Ok(BlobRef {
        key: read_key(reader, limits)?,
        plaintext_len: reader.read_u64()?,
        stored_len: reader.read_u64()?,
        representation: read_representation(reader)?,
        digest: Digest::new(reader.read_array()?),
    })
}

fn write_representation(writer: &mut Writer, representation: Representation) {
    writer.write_u8(match representation.hash() {
        HashAlgorithm::Blake3_256 => HASH_BLAKE3_256,
    });
    writer.write_u8(match representation.codec() {
        PayloadCodec::Identity => CODEC_IDENTITY,
    });
    writer.write_u8(match representation.cipher() {
        PayloadCipher::None => CIPHER_NONE,
    });
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
    Ok(Representation::CURRENT)
}
