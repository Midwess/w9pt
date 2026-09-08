//! Deterministic compression, protection, and managed per-file key contexts.

mod compression;
mod encryption;
mod file_keys;

use core::fmt;
use core::ops::{Deref, DerefMut};

use zeroize::Zeroizing;

use crate::{Digest, FileId, ObjectKey, PreparationIdentity, StorageMethod};

pub use file_keys::{
    CandidateKeyError, ContentMetadataCandidate, FileContextScope, MasterKey, MasterKeyId,
    SecureEntropy, generate_content_metadata, open_committed_context, rewrap_content_metadata,
};

const OBJECT_MAGIC: [u8; 8] = *b"W9PTREP\0";
const BODY_MAGIC: [u8; 8] = *b"W9PTBODY";
const FORMAT_MAJOR: u16 = 3;
const FORMAT_MINOR: u16 = 0;
const OUTER_HEADER_BYTES: usize = 8 + 1 + 2 + 2 + 1 + 16 + 8;
const PLAIN_CHECKSUM_BYTES: usize = 32;
const SIV_TAG_BYTES: usize = 16;
const BODY_FIXED_BYTES: usize = 83;
const AAD_FIXED_BYTES: usize = OUTER_HEADER_BYTES + 8 + 2 + 2 + 4;
const OBJECT_KEY_BYTES: usize = 64;
pub(crate) const RAW_PAYLOAD_MAX_OVERHEAD: u64 = 246;
pub(crate) const BLOCK_PAYLOAD_MAX_OVERHEAD: u64 = 254;
pub(crate) const PAYLOAD_MIN_STORED_BYTES: u64 = 231;
pub(crate) const MAP_PAGE_MIN_OVERHEAD: usize = 239;

enum ScratchBytes {
    Plain(Vec<u8>),
    Secret(Zeroizing<Vec<u8>>),
}

impl ScratchBytes {
    fn new(bytes: Vec<u8>, secret: bool) -> Self {
        if secret {
            Self::Secret(Zeroizing::new(bytes))
        } else {
            Self::Plain(bytes)
        }
    }
}

impl Deref for ScratchBytes {
    type Target = Vec<u8>;

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Plain(bytes) => bytes,
            Self::Secret(bytes) => bytes,
        }
    }
}

impl DerefMut for ScratchBytes {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self {
            Self::Plain(bytes) => bytes,
            Self::Secret(bytes) => bytes,
        }
    }
}

/// Compression policy pinned for one file.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CompressionPolicy {
    /// Store actual payload bytes unchanged.
    Identity,
    /// Try the frozen safe LZ4 block encoder and require deterministic output.
    Lz4BlockV1,
}

/// Immutable-object protection policy pinned for one file.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ContentCipher {
    /// Store checked plaintext bodies.
    None,
    /// Protect bodies with deterministic RFC 5297 AES-256-SIV.
    Aes256SivV1,
}

impl ContentCipher {
    /// Reports whether content requires a file DEK.
    pub const fn is_encrypted(self) -> bool {
        matches!(self, Self::Aes256SivV1)
    }
}

/// Pinned file layout, compression, and content-protection policy.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FileStoragePolicy {
    method: StorageMethod,
    compression: CompressionPolicy,
    encryption: ContentCipher,
}

impl FileStoragePolicy {
    /// Opaque policy format supplied to generic state metadata.
    pub const FORMAT: u16 = 1;

    /// Constructs a supported policy.
    pub const fn new(
        method: StorageMethod,
        compression: CompressionPolicy,
        encryption: ContentCipher,
    ) -> Self {
        Self {
            method,
            compression,
            encryption,
        }
    }

    /// Constructs the default uncompressed/unencrypted policy.
    pub const fn plain(method: StorageMethod) -> Self {
        Self::new(method, CompressionPolicy::Identity, ContentCipher::None)
    }

    /// Returns the logical storage method.
    pub const fn method(self) -> StorageMethod {
        self.method
    }
    /// Returns the requested compression policy.
    pub const fn compression(self) -> CompressionPolicy {
        self.compression
    }
    /// Returns the content protection policy.
    pub const fn encryption(self) -> ContentCipher {
        self.encryption
    }

    /// Returns the canonical opaque state bytes.
    pub const fn to_bytes(self) -> [u8; 4] {
        [
            1,
            match self.method {
                StorageMethod::Raw => 1,
                StorageMethod::BlockSplit => 2,
            },
            match self.compression {
                CompressionPolicy::Identity => 0,
                CompressionPolicy::Lz4BlockV1 => 1,
            },
            match self.encryption {
                ContentCipher::None => 0,
                ContentCipher::Aes256SivV1 => 1,
            },
        ]
    }

    /// Parses canonical opaque policy bytes.
    pub fn from_bytes(format: u16, bytes: &[u8]) -> Result<Self, RepresentationError> {
        if format != Self::FORMAT || bytes.len() != 4 || bytes[0] != 1 {
            return Err(RepresentationError::UnsupportedPolicy);
        }
        let method = match bytes[1] {
            1 => StorageMethod::Raw,
            2 => StorageMethod::BlockSplit,
            _ => return Err(RepresentationError::UnsupportedPolicy),
        };
        let compression = match bytes[2] {
            0 => CompressionPolicy::Identity,
            1 => CompressionPolicy::Lz4BlockV1,
            _ => return Err(RepresentationError::UnsupportedCompression),
        };
        let encryption = match bytes[3] {
            0 => ContentCipher::None,
            1 => ContentCipher::Aes256SivV1,
            _ => return Err(RepresentationError::UnsupportedCipher),
        };
        Ok(Self::new(method, compression, encryption))
    }
}

pub(crate) fn require_writer_support(policy: FileStoragePolicy) -> Result<(), RepresentationError> {
    if matches!(policy.compression(), CompressionPolicy::Lz4BlockV1)
        && !cfg!(feature = "compression-lz4")
    {
        return Err(RepresentationError::UnsupportedCompression);
    }
    if matches!(policy.encryption(), ContentCipher::Aes256SivV1)
        && !cfg!(feature = "encryption-aes-siv")
    {
        return Err(RepresentationError::UnsupportedCipher);
    }
    Ok(())
}

/// Stable nonsecret identity of one file's content context and DEK.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ContentContextId([u8; 16]);

impl ContentContextId {
    /// Constructs an ID from canonical bytes.
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }
    /// Returns canonical bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
    /// Creates an ID from an integer for deterministic fixtures.
    pub const fn from_u128(value: u128) -> Self {
        Self(value.to_be_bytes())
    }
}

impl fmt::Debug for ContentContextId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ContentContextId(..)")
    }
}

/// Nonsecret context binding carried by prepared content into state publication.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContentContextBinding {
    file_id: FileId,
    context_id: ContentContextId,
    policy_format: u16,
    policy_bytes: Box<[u8]>,
    key_commitment: Option<[u8; 32]>,
    context_revision: u64,
}

impl ContentContextBinding {
    /// Constructs a bounded binding.
    pub fn new(
        file_id: FileId,
        context_id: ContentContextId,
        policy_format: u16,
        policy_bytes: Vec<u8>,
        key_commitment: Option<[u8; 32]>,
        context_revision: u64,
    ) -> Result<Self, RepresentationError> {
        if policy_format == 0 || policy_bytes.is_empty() || policy_bytes.len() > 512 {
            return Err(RepresentationError::InvalidPolicy);
        }
        Ok(Self {
            file_id,
            context_id,
            policy_format,
            policy_bytes: policy_bytes.into_boxed_slice(),
            key_commitment,
            context_revision,
        })
    }

    /// Returns the content file.
    pub const fn file_id(&self) -> FileId {
        self.file_id
    }
    /// Returns the context identity.
    pub const fn context_id(&self) -> ContentContextId {
        self.context_id
    }
    /// Returns the opaque policy format.
    pub const fn policy_format(&self) -> u16 {
        self.policy_format
    }
    /// Returns exact policy bytes.
    pub fn policy_bytes(&self) -> &[u8] {
        &self.policy_bytes
    }
    /// Returns the optional stable key commitment.
    pub const fn key_commitment(&self) -> Option<&[u8; 32]> {
        self.key_commitment.as_ref()
    }
    /// Returns the selected authoritative metadata revision.
    pub const fn context_revision(&self) -> u64 {
        self.context_revision
    }
}

/// One operation-scoped selected file context with redacted diagnostics.
pub struct FileCryptoContext {
    binding: ContentContextBinding,
    policy: FileStoragePolicy,
    dek: Option<Zeroizing<[u8; 32]>>,
}

impl FileCryptoContext {
    pub(super) fn new(
        binding: ContentContextBinding,
        policy: FileStoragePolicy,
        dek: Option<Zeroizing<[u8; 32]>>,
    ) -> Self {
        Self {
            binding,
            policy,
            dek,
        }
    }

    /// Returns the nonsecret publication binding.
    pub const fn binding(&self) -> &ContentContextBinding {
        &self.binding
    }
    /// Returns the pinned storage policy.
    pub const fn policy(&self) -> FileStoragePolicy {
        self.policy
    }
    pub(super) fn dek(&self) -> Result<&[u8; 32], RepresentationError> {
        self.dek
            .as_deref()
            .ok_or(RepresentationError::MissingFileKey)
    }

    /// Constructs the deterministic plain context used by standalone repository helpers.
    pub fn plain_for_file(file_id: FileId, method: StorageMethod) -> Self {
        file_keys::implicit_plain_context(file_id, method)
    }

    pub(crate) fn preparation_token(
        &self,
        identity: PreparationIdentity,
        attempt: u32,
    ) -> [u8; 32] {
        let mut material = Vec::with_capacity(16 + 16 + 8 + 32 + 32 + 4 + 4);
        material.extend_from_slice(self.binding.file_id.as_bytes());
        material.extend_from_slice(self.binding.context_id.as_bytes());
        material.extend_from_slice(identity.mutation_id().as_bytes());
        material.extend_from_slice(&identity.base().generation().to_le_bytes());
        material.extend_from_slice(identity.base().manifest_digest().as_bytes());
        material.extend_from_slice(identity.fingerprint().as_bytes());
        material.extend_from_slice(&attempt.to_le_bytes());
        material.extend_from_slice(&self.policy.to_bytes());
        match &self.dek {
            Some(dek) => encryption::derive(
                "w9pt v3 protected preparation token",
                &[dek.as_ref(), &material],
            ),
            None => encryption::derive("w9pt v3 plain preparation token", &[&material]),
        }
    }
}

impl fmt::Debug for FileCryptoContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FileCryptoContext")
            .field("binding", &self.binding)
            .field("policy", &self.policy)
            .field("dek", &self.dek.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// Actual payload codec recorded inside a checked object body.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActualCodec {
    /// Canonical bytes are stored directly.
    Identity,
    /// Canonical bytes use the frozen LZ4 block profile.
    Lz4BlockV1,
}

/// Provenance authenticated inside one immutable object.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjectProvenance {
    /// Compact file manifest.
    Manifest {
        /// Stable preparation identity.
        identity: PreparationIdentity,
        /// Collision-recovery attempt.
        attempt: u32,
        /// Published content generation.
        generation: u64,
    },
    /// Raw whole-file payload.
    RawPayload {
        /// Stable preparation identity.
        identity: PreparationIdentity,
        /// Collision-recovery attempt.
        attempt: u32,
    },
    /// One block payload.
    BlockPayload {
        /// Stable preparation identity.
        identity: PreparationIdentity,
        /// Collision-recovery attempt.
        attempt: u32,
        /// File-relative logical block index.
        block_index: u64,
    },
    /// One immutable map page.
    MapPage {
        /// Stable preparation identity.
        identity: PreparationIdentity,
        /// Collision-recovery attempt.
        attempt: u32,
        /// Map-tree level.
        level: u8,
        /// First logical block covered by the page.
        first_block: u64,
    },
}

impl ObjectProvenance {
    pub(crate) const fn identity_attempt(self) -> (PreparationIdentity, u32) {
        match self {
            Self::Manifest {
                identity, attempt, ..
            }
            | Self::RawPayload { identity, attempt }
            | Self::BlockPayload {
                identity, attempt, ..
            }
            | Self::MapPage {
                identity, attempt, ..
            } => (identity, attempt),
        }
    }

    const fn encoded_len(self) -> usize {
        let suffix = match self {
            Self::Manifest { .. } | Self::BlockPayload { .. } => 8,
            Self::RawPayload { .. } => 0,
            Self::MapPage { .. } => 9,
        };
        1 + 16 + 8 + 32 + 32 + 4 + suffix
    }
}

pub(crate) fn encode_working_bytes(
    canonical_len: usize,
    key: &ObjectKey,
    provenance: ObjectProvenance,
    context: &FileCryptoContext,
    payload_compression: bool,
) -> Result<usize, RepresentationError> {
    let codec_capacity = if payload_compression {
        compression::maximum_encode_capacity(context.policy.compression(), canonical_len)?
    } else {
        canonical_len
    };
    let provenance_len = provenance.encoded_len();
    let body_len = BODY_FIXED_BYTES
        .checked_add(provenance_len)
        .and_then(|size| size.checked_add(canonical_len))
        .ok_or(RepresentationError::InvalidLength)?;
    let protection_bytes = if context.policy.encryption().is_encrypted() {
        SIV_TAG_BYTES
    } else {
        PLAIN_CHECKSUM_BYTES
    };
    let protected_len = protection_bytes
        .checked_add(body_len)
        .ok_or(RepresentationError::InvalidLength)?;
    let final_len = OUTER_HEADER_BYTES
        .checked_add(protected_len)
        .ok_or(RepresentationError::InvalidLength)?;
    let aad_len = AAD_FIXED_BYTES
        .checked_add(key.as_str().len())
        .ok_or(RepresentationError::InvalidLength)?;
    let key_bytes = if context.policy.encryption().is_encrypted() {
        OBJECT_KEY_BYTES
    } else {
        0
    };
    checked_sum(&[
        codec_capacity,
        provenance_len,
        body_len,
        aad_len,
        protected_len,
        final_len,
        key_bytes,
    ])
}

pub(crate) fn decode_working_bytes(
    canonical_len: usize,
    stored_len: usize,
    key: &ObjectKey,
    context: &FileCryptoContext,
) -> Result<usize, RepresentationError> {
    let aad_len = AAD_FIXED_BYTES
        .checked_add(key.as_str().len())
        .ok_or(RepresentationError::InvalidLength)?;
    let key_bytes = if context.policy.encryption().is_encrypted() {
        OBJECT_KEY_BYTES
    } else {
        0
    };
    checked_sum(&[stored_len, stored_len, canonical_len, aad_len, key_bytes])
}

fn checked_sum(values: &[usize]) -> Result<usize, RepresentationError> {
    values.iter().try_fold(0usize, |total, value| {
        total
            .checked_add(*value)
            .ok_or(RepresentationError::InvalidLength)
    })
}

/// Successfully decoded canonical object bytes and authenticated provenance.
#[derive(Debug)]
pub(crate) struct DecodedObject {
    pub(crate) canonical: Vec<u8>,
    pub(crate) provenance: ObjectProvenance,
}

/// Representation, key, codec, or authentication failure without secret diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RepresentationError {
    /// Opaque storage policy is malformed.
    InvalidPolicy,
    /// Policy version/tag is unsupported.
    UnsupportedPolicy,
    /// Compression implementation/profile is unavailable.
    UnsupportedCompression,
    /// Cipher implementation/profile is unavailable.
    UnsupportedCipher,
    /// Codec execution or validation failed.
    CodecFailure,
    /// A checked representation length is inconsistent.
    InvalidLength,
    /// Protected authentication failed.
    AuthenticationFailed,
    /// Required committed file context is absent.
    MissingContext,
    /// Required master wrapping key was not supplied.
    MissingMaster,
    /// Required file DEK was not supplied.
    MissingFileKey,
    /// Master identity does not select the wrapped record.
    WrongMaster,
    /// Context, file, policy, or optional-field binding differs.
    ContextMismatch,
    /// Unwrapped DEK does not match its stable commitment.
    CommitmentMismatch,
    /// Wrapped-key bytes are malformed.
    InvalidWrappedKey,
    /// Wrapped-key format is unsupported.
    UnsupportedWrappedKey,
    /// Wrapping execution failed.
    WrappingFailed,
    /// Authenticated object provenance differs from the selected key/location.
    ProvenanceMismatch,
}

impl fmt::Display for RepresentationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidPolicy => "invalid storage policy",
            Self::UnsupportedPolicy => "unsupported storage policy",
            Self::UnsupportedCompression => "unsupported compression profile",
            Self::UnsupportedCipher => "unsupported content cipher",
            Self::CodecFailure => "content codec failed",
            Self::InvalidLength => "invalid representation length",
            Self::AuthenticationFailed => "content authentication failed",
            Self::MissingContext => "content context is missing",
            Self::MissingMaster => "master key is missing",
            Self::MissingFileKey => "file key is missing",
            Self::WrongMaster => "wrong master key",
            Self::ContextMismatch => "content context mismatch",
            Self::CommitmentMismatch => "file key commitment mismatch",
            Self::InvalidWrappedKey => "invalid wrapped key",
            Self::UnsupportedWrappedKey => "unsupported wrapped-key format",
            Self::WrappingFailed => "file key wrapping failed",
            Self::ProvenanceMismatch => "object provenance mismatch",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for RepresentationError {}

/// Encodes deterministic checked target bytes for one canonical object.
/// Encodes one complete deterministic v3 immutable target object.
pub fn encode_object(
    kind: crate::format::ObjectKind,
    key: &ObjectKey,
    canonical: &[u8],
    provenance: ObjectProvenance,
    context: &FileCryptoContext,
    payload_compression: bool,
    max_stored: usize,
) -> Result<Vec<u8>, RepresentationError> {
    let (codec, encoded) = if payload_compression {
        compression::encode(context.policy.compression, canonical)?
    } else {
        (ActualCodec::Identity, canonical.to_vec())
    };
    let secret = context.policy.encryption().is_encrypted();
    let encoded = ScratchBytes::new(encoded, secret);
    let provenance = encode_provenance(provenance);
    let policy = context.policy.to_bytes();
    let mut body = ScratchBytes::new(Vec::new(), secret);
    body.extend_from_slice(&BODY_MAGIC);
    body.extend_from_slice(
        &u16::try_from(provenance.len())
            .map_err(|_| RepresentationError::InvalidLength)?
            .to_le_bytes(),
    );
    body.extend_from_slice(&provenance);
    body.extend_from_slice(&FileStoragePolicy::FORMAT.to_le_bytes());
    body.extend_from_slice(
        &u16::try_from(policy.len())
            .map_err(|_| RepresentationError::InvalidLength)?
            .to_le_bytes(),
    );
    body.extend_from_slice(&policy);
    body.extend_from_slice(context.binding.context_id.as_bytes());
    body.push(match codec {
        ActualCodec::Identity => 0,
        ActualCodec::Lz4BlockV1 => 1,
    });
    body.extend_from_slice(
        &u64::try_from(canonical.len())
            .map_err(|_| RepresentationError::InvalidLength)?
            .to_le_bytes(),
    );
    body.extend_from_slice(
        &u64::try_from(encoded.len())
            .map_err(|_| RepresentationError::InvalidLength)?
            .to_le_bytes(),
    );
    body.extend_from_slice(Digest::blake3(canonical).as_bytes());
    body.extend_from_slice(&encoded);

    let cipher_tag = match context.policy.encryption {
        ContentCipher::None => 0,
        ContentCipher::Aes256SivV1 => 1,
    };
    let protected_len = match context.policy.encryption {
        ContentCipher::None => PLAIN_CHECKSUM_BYTES.checked_add(body.len()),
        ContentCipher::Aes256SivV1 => SIV_TAG_BYTES.checked_add(body.len()),
    }
    .ok_or(RepresentationError::InvalidLength)?;
    let total = OUTER_HEADER_BYTES
        .checked_add(protected_len)
        .ok_or(RepresentationError::InvalidLength)?;
    if total > max_stored {
        return Err(RepresentationError::InvalidLength);
    }
    let mut header = Vec::with_capacity(OUTER_HEADER_BYTES);
    header.extend_from_slice(&OBJECT_MAGIC);
    header.push(kind as u8);
    header.extend_from_slice(&FORMAT_MAJOR.to_le_bytes());
    header.extend_from_slice(&FORMAT_MINOR.to_le_bytes());
    header.push(cipher_tag);
    header.extend_from_slice(context.binding.context_id.as_bytes());
    header.extend_from_slice(
        &u64::try_from(protected_len)
            .map_err(|_| RepresentationError::InvalidLength)?
            .to_le_bytes(),
    );
    let aad = object_aad(&header, key, FileStoragePolicy::FORMAT, &policy)?;
    let protected = match context.policy.encryption {
        ContentCipher::None => {
            let mut output = Vec::with_capacity(protected_len);
            output.extend_from_slice(
                Digest::blake3(&[aad.as_slice(), body.as_slice()].concat()).as_bytes(),
            );
            output.extend_from_slice(&body);
            output
        }
        ContentCipher::Aes256SivV1 => {
            encryption::encrypt(context.dek()?, key.as_str().as_bytes(), &aad, &body)?
        }
    };
    header.extend_from_slice(&protected);
    Ok(header)
}

/// Decodes and authenticates one target object before returning canonical bytes.
pub(crate) fn decode_object(
    expected_kind: crate::format::ObjectKind,
    key: &ObjectKey,
    stored: &[u8],
    context: &FileCryptoContext,
    max_canonical: usize,
) -> Result<DecodedObject, RepresentationError> {
    if stored.len() < OUTER_HEADER_BYTES {
        return Err(RepresentationError::InvalidLength);
    }
    if stored[..8] != OBJECT_MAGIC {
        return Err(RepresentationError::UnsupportedPolicy);
    }
    if stored[8] != expected_kind as u8 {
        return Err(RepresentationError::ProvenanceMismatch);
    }
    if u16::from_le_bytes(stored[9..11].try_into().expect("fixed slice")) != FORMAT_MAJOR
        || u16::from_le_bytes(stored[11..13].try_into().expect("fixed slice")) != FORMAT_MINOR
    {
        return Err(RepresentationError::UnsupportedPolicy);
    }
    let cipher = match stored[13] {
        0 => ContentCipher::None,
        1 => ContentCipher::Aes256SivV1,
        _ => return Err(RepresentationError::UnsupportedCipher),
    };
    if cipher != context.policy.encryption
        || stored[14..30] != *context.binding.context_id.as_bytes()
    {
        return Err(RepresentationError::ContextMismatch);
    }
    let body_len = usize::try_from(u64::from_le_bytes(
        stored[30..38].try_into().expect("fixed slice"),
    ))
    .map_err(|_| RepresentationError::InvalidLength)?;
    if stored.len()
        != OUTER_HEADER_BYTES
            .checked_add(body_len)
            .ok_or(RepresentationError::InvalidLength)?
    {
        return Err(RepresentationError::InvalidLength);
    }
    let policy = context.policy.to_bytes();
    let aad = object_aad(
        &stored[..OUTER_HEADER_BYTES],
        key,
        FileStoragePolicy::FORMAT,
        &policy,
    )?;
    let protected = &stored[OUTER_HEADER_BYTES..];
    let body = ScratchBytes::new(
        match cipher {
            ContentCipher::None => {
                if protected.len() < PLAIN_CHECKSUM_BYTES {
                    return Err(RepresentationError::InvalidLength);
                }
                let expected = &protected[..PLAIN_CHECKSUM_BYTES];
                let actual =
                    Digest::blake3(&[aad.as_slice(), &protected[PLAIN_CHECKSUM_BYTES..]].concat());
                if !constant_time_eq(expected, actual.as_bytes()) {
                    return Err(RepresentationError::AuthenticationFailed);
                }
                protected[PLAIN_CHECKSUM_BYTES..].to_vec()
            }
            ContentCipher::Aes256SivV1 => {
                encryption::decrypt(context.dek()?, key.as_str().as_bytes(), &aad, protected)?
            }
        },
        cipher.is_encrypted(),
    );
    decode_body(&body, context, max_canonical)
}

fn decode_body(
    body: &[u8],
    context: &FileCryptoContext,
    max_canonical: usize,
) -> Result<DecodedObject, RepresentationError> {
    let mut position = 0;
    let take = |position: &mut usize, length: usize| -> Result<&[u8], RepresentationError> {
        let end = position
            .checked_add(length)
            .ok_or(RepresentationError::InvalidLength)?;
        let bytes = body
            .get(*position..end)
            .ok_or(RepresentationError::InvalidLength)?;
        *position = end;
        Ok(bytes)
    };
    if take(&mut position, 8)? != BODY_MAGIC {
        return Err(RepresentationError::ProvenanceMismatch);
    }
    let provenance_len = usize::from(u16::from_le_bytes(
        take(&mut position, 2)?.try_into().expect("fixed slice"),
    ));
    let provenance = decode_provenance(take(&mut position, provenance_len)?)?;
    let policy_format =
        u16::from_le_bytes(take(&mut position, 2)?.try_into().expect("fixed slice"));
    let policy_len = usize::from(u16::from_le_bytes(
        take(&mut position, 2)?.try_into().expect("fixed slice"),
    ));
    let policy = take(&mut position, policy_len)?;
    if policy_format != context.binding.policy_format || policy != context.binding.policy_bytes() {
        return Err(RepresentationError::ContextMismatch);
    }
    if take(&mut position, 16)? != context.binding.context_id.as_bytes() {
        return Err(RepresentationError::ContextMismatch);
    }
    let codec = match take(&mut position, 1)?[0] {
        0 => ActualCodec::Identity,
        1 => ActualCodec::Lz4BlockV1,
        _ => return Err(RepresentationError::UnsupportedCompression),
    };
    let canonical_len = usize::try_from(u64::from_le_bytes(
        take(&mut position, 8)?.try_into().expect("fixed slice"),
    ))
    .map_err(|_| RepresentationError::InvalidLength)?;
    let encoded_len = usize::try_from(u64::from_le_bytes(
        take(&mut position, 8)?.try_into().expect("fixed slice"),
    ))
    .map_err(|_| RepresentationError::InvalidLength)?;
    if canonical_len > max_canonical || body.len().saturating_sub(position) < 32 {
        return Err(RepresentationError::InvalidLength);
    }
    let digest = take(&mut position, 32)?;
    let encoded = take(&mut position, encoded_len)?;
    if position != body.len() {
        return Err(RepresentationError::InvalidLength);
    }
    let canonical = compression::decode(codec, encoded, canonical_len)?;
    let actual = Digest::blake3(&canonical);
    if !constant_time_eq(digest, actual.as_bytes()) {
        return Err(RepresentationError::AuthenticationFailed);
    }
    Ok(DecodedObject {
        canonical,
        provenance,
    })
}

fn object_aad(
    header: &[u8],
    key: &ObjectKey,
    policy_format: u16,
    policy: &[u8],
) -> Result<Vec<u8>, RepresentationError> {
    let mut aad = Vec::with_capacity(header.len() + key.as_str().len() + policy.len() + 12);
    aad.extend_from_slice(header);
    aad.extend_from_slice(
        &u64::try_from(key.as_str().len())
            .map_err(|_| RepresentationError::InvalidLength)?
            .to_le_bytes(),
    );
    aad.extend_from_slice(key.as_str().as_bytes());
    aad.extend_from_slice(&policy_format.to_le_bytes());
    aad.extend_from_slice(
        &u16::try_from(policy.len())
            .map_err(|_| RepresentationError::InvalidLength)?
            .to_le_bytes(),
    );
    aad.extend_from_slice(policy);
    Ok(aad)
}

fn encode_provenance(provenance: ObjectProvenance) -> Vec<u8> {
    let (tag, identity, attempt, suffix) = match provenance {
        ObjectProvenance::Manifest {
            identity,
            attempt,
            generation,
        } => (1, identity, attempt, generation.to_le_bytes().to_vec()),
        ObjectProvenance::RawPayload { identity, attempt } => (2, identity, attempt, Vec::new()),
        ObjectProvenance::BlockPayload {
            identity,
            attempt,
            block_index,
        } => (3, identity, attempt, block_index.to_le_bytes().to_vec()),
        ObjectProvenance::MapPage {
            identity,
            attempt,
            level,
            first_block,
        } => {
            let mut suffix = vec![level];
            suffix.extend_from_slice(&first_block.to_le_bytes());
            (4, identity, attempt, suffix)
        }
    };
    let mut bytes = Vec::with_capacity(1 + 16 + 8 + 32 + 32 + 4 + suffix.len());
    bytes.push(tag);
    bytes.extend_from_slice(identity.mutation_id().as_bytes());
    bytes.extend_from_slice(&identity.base().generation().to_le_bytes());
    bytes.extend_from_slice(identity.base().manifest_digest().as_bytes());
    bytes.extend_from_slice(identity.fingerprint().as_bytes());
    bytes.extend_from_slice(&attempt.to_le_bytes());
    bytes.extend_from_slice(&suffix);
    bytes
}

fn decode_provenance(bytes: &[u8]) -> Result<ObjectProvenance, RepresentationError> {
    const COMMON: usize = 1 + 16 + 8 + 32 + 32 + 4;
    if bytes.len() < COMMON {
        return Err(RepresentationError::ProvenanceMismatch);
    }
    let tag = bytes[0];
    let mutation = crate::MutationId::new(bytes[1..17].try_into().expect("fixed slice"));
    let generation = u64::from_le_bytes(bytes[17..25].try_into().expect("fixed slice"));
    let digest = Digest::new(bytes[25..57].try_into().expect("fixed slice"));
    let fingerprint =
        crate::OperationFingerprint::new(bytes[57..89].try_into().expect("fixed slice"));
    let attempt = u32::from_le_bytes(bytes[89..93].try_into().expect("fixed slice"));
    let identity = PreparationIdentity::new(
        mutation,
        crate::BaseContentIdentity::new(generation, digest),
        fingerprint,
    );
    match (tag, &bytes[COMMON..]) {
        (1, suffix) if suffix.len() == 8 => Ok(ObjectProvenance::Manifest {
            identity,
            attempt,
            generation: u64::from_le_bytes(suffix.try_into().expect("fixed slice")),
        }),
        (2, []) => Ok(ObjectProvenance::RawPayload { identity, attempt }),
        (3, suffix) if suffix.len() == 8 => Ok(ObjectProvenance::BlockPayload {
            identity,
            attempt,
            block_index: u64::from_le_bytes(suffix.try_into().expect("fixed slice")),
        }),
        (4, suffix) if suffix.len() == 9 => Ok(ObjectProvenance::MapPage {
            identity,
            attempt,
            level: suffix[0],
            first_block: u64::from_le_bytes(suffix[1..].try_into().expect("fixed slice")),
        }),
        _ => Err(RepresentationError::ProvenanceMismatch),
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0_u8, |difference, (left, right)| {
                difference | (left ^ right)
            })
            == 0
}
