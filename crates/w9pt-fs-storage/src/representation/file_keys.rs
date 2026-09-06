use core::fmt;

use zeroize::{Zeroize, Zeroizing};

use crate::{FileId, RepresentationError, StorageMethod};

use super::{
    ContentContextBinding, ContentContextId, FileCryptoContext, FileStoragePolicy,
    encryption::derive,
};

const WRAP_MAGIC: [u8; 8] = *b"W9PTKEY\0";
const WRAP_VERSION: u16 = 1;
const DEK_BYTES: usize = 32;
const MASTER_ID_BYTES: usize = 16;
const WRAPPED_DEK_BYTES: usize = DEK_BYTES + 16;

/// Stable public identity of one externally managed master wrapping key.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MasterKeyId([u8; MASTER_ID_BYTES]);

impl MasterKeyId {
    /// Constructs an identifier from its canonical bytes.
    pub const fn new(bytes: [u8; MASTER_ID_BYTES]) -> Self {
        Self(bytes)
    }
    /// Returns the canonical identifier bytes.
    pub const fn as_bytes(&self) -> &[u8; MASTER_ID_BYTES] {
        &self.0
    }
}

impl fmt::Debug for MasterKeyId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("MasterKeyId(..)")
    }
}

/// Caller-owned high-entropy master wrapping key with redacted diagnostics.
pub struct MasterKey {
    id: MasterKeyId,
    bytes: Zeroizing<[u8; 32]>,
}

impl MasterKey {
    /// Wraps an identified 32-byte master key.
    pub fn new(id: MasterKeyId, bytes: [u8; 32]) -> Self {
        Self {
            id,
            bytes: Zeroizing::new(bytes),
        }
    }
    /// Returns the public master-key identity.
    pub const fn id(&self) -> MasterKeyId {
        self.id
    }
    pub(super) fn bytes(&self) -> &[u8; 32] {
        &self.bytes
    }
}

impl fmt::Debug for MasterKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MasterKey")
            .field("id", &self.id)
            .field("bytes", &"<redacted>")
            .finish()
    }
}

/// Caller-provided source of cryptographically secure random bytes.
pub trait SecureEntropy {
    /// Caller-specific entropy failure.
    type Error: std::error::Error + Send + Sync + 'static;
    /// Fills the complete destination or returns an error.
    fn fill_secure(&mut self, destination: &mut [u8]) -> Result<(), Self::Error>;
}

/// Public file/context scope bound into wrapping and commitments.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FileContextScope {
    filesystem_id: [u8; 16],
    owner_inode_id: [u8; 16],
    file_id: FileId,
    context_id: ContentContextId,
}

impl FileContextScope {
    /// Constructs a complete stable context scope.
    pub const fn new(
        filesystem_id: [u8; 16],
        owner_inode_id: [u8; 16],
        file_id: FileId,
        context_id: ContentContextId,
    ) -> Self {
        Self {
            filesystem_id,
            owner_inode_id,
            file_id,
            context_id,
        }
    }
    /// Returns the filesystem identity bytes.
    pub const fn filesystem_id(&self) -> &[u8; 16] {
        &self.filesystem_id
    }
    /// Returns the original owner inode identity bytes.
    pub const fn owner_inode_id(&self) -> &[u8; 16] {
        &self.owner_inode_id
    }
    /// Returns the content file identity.
    pub const fn file_id(&self) -> FileId {
        self.file_id
    }
    /// Returns the stable content context identity.
    pub const fn context_id(&self) -> ContentContextId {
        self.context_id
    }
}

/// Bounded opaque metadata proposed for authoritative state insertion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContentMetadataCandidate {
    policy_format: u16,
    policy_bytes: Box<[u8]>,
    key_commitment: Option<[u8; 32]>,
    wrapped_key_bytes: Option<Box<[u8]>>,
}

impl ContentMetadataCandidate {
    /// Returns the opaque policy format.
    pub const fn policy_format(&self) -> u16 {
        self.policy_format
    }
    /// Returns the canonical opaque policy bytes.
    pub fn policy_bytes(&self) -> &[u8] {
        &self.policy_bytes
    }
    /// Returns the immutable key commitment for encrypted contexts.
    pub const fn key_commitment(&self) -> Option<&[u8; 32]> {
        self.key_commitment.as_ref()
    }
    /// Returns the opaque wrapped key envelope.
    pub fn wrapped_key_bytes(&self) -> Option<&[u8]> {
        self.wrapped_key_bytes.as_deref()
    }
}

/// Failure while generating candidate metadata.
#[derive(Debug)]
pub enum CandidateKeyError<E> {
    /// The caller-owned entropy source failed.
    Entropy(E),
    /// Policy, wrapping, or representation validation failed.
    Representation(RepresentationError),
    /// An encrypted policy was requested without a master key.
    MissingMaster,
}

impl<E: fmt::Display> fmt::Display for CandidateKeyError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Entropy(error) => write!(formatter, "secure entropy failed: {error}"),
            Self::Representation(error) => error.fmt(formatter),
            Self::MissingMaster => formatter.write_str("encrypted content requires a master key"),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for CandidateKeyError<E> {}

/// Generates bounded opaque metadata for one not-yet-authoritative file context.
pub fn generate_content_metadata<E: SecureEntropy>(
    scope: FileContextScope,
    policy: FileStoragePolicy,
    master: Option<&MasterKey>,
    entropy: &mut E,
) -> Result<ContentMetadataCandidate, CandidateKeyError<E::Error>> {
    let policy_bytes = policy.to_bytes();
    if !policy.encryption().is_encrypted() {
        return Ok(ContentMetadataCandidate {
            policy_format: FileStoragePolicy::FORMAT,
            policy_bytes: policy_bytes.into(),
            key_commitment: None,
            wrapped_key_bytes: None,
        });
    }
    let master = master.ok_or(CandidateKeyError::MissingMaster)?;
    let mut dek = Zeroizing::new([0; DEK_BYTES]);
    entropy
        .fill_secure(dek.as_mut())
        .map_err(CandidateKeyError::Entropy)?;
    let commitment = commitment(&dek, scope, &policy_bytes);
    let wrapped = wrap_dek(master, &dek, scope, &policy_bytes, &commitment)
        .map_err(CandidateKeyError::Representation)?;
    Ok(ContentMetadataCandidate {
        policy_format: FileStoragePolicy::FORMAT,
        policy_bytes: policy_bytes.into(),
        key_commitment: Some(commitment),
        wrapped_key_bytes: Some(wrapped.into_boxed_slice()),
    })
}

/// Opens the exact committed opaque metadata into one operation-scoped context.
#[allow(clippy::too_many_arguments)]
pub fn open_committed_context(
    scope: FileContextScope,
    policy_format: u16,
    policy_bytes: &[u8],
    key_commitment: Option<[u8; 32]>,
    wrapped_key_bytes: Option<&[u8]>,
    context_revision: u64,
    master: Option<&MasterKey>,
) -> Result<FileCryptoContext, RepresentationError> {
    let policy = FileStoragePolicy::from_bytes(policy_format, policy_bytes)?;
    let binding = ContentContextBinding::new(
        scope.file_id,
        scope.context_id,
        policy_format,
        policy_bytes.to_vec(),
        key_commitment,
        context_revision,
    )?;
    if !policy.encryption().is_encrypted() {
        if key_commitment.is_some() || wrapped_key_bytes.is_some() {
            return Err(RepresentationError::ContextMismatch);
        }
        return Ok(FileCryptoContext::new(binding, policy, None));
    }
    let master = master.ok_or(RepresentationError::MissingMaster)?;
    let expected = key_commitment.ok_or(RepresentationError::ContextMismatch)?;
    let wrapped = wrapped_key_bytes.ok_or(RepresentationError::ContextMismatch)?;
    let dek = unwrap_dek(master, wrapped, scope, policy_bytes, &expected)?;
    Ok(FileCryptoContext::new(binding, policy, Some(dek)))
}

/// Rewraps the same committed DEK under a new identified master key.
pub fn rewrap_content_metadata(
    scope: FileContextScope,
    policy_format: u16,
    policy_bytes: &[u8],
    key_commitment: [u8; 32],
    wrapped_key_bytes: &[u8],
    old_master: &MasterKey,
    new_master: &MasterKey,
) -> Result<Vec<u8>, RepresentationError> {
    let policy = FileStoragePolicy::from_bytes(policy_format, policy_bytes)?;
    if !policy.encryption().is_encrypted() {
        return Err(RepresentationError::ContextMismatch);
    }
    let dek = unwrap_dek(
        old_master,
        wrapped_key_bytes,
        scope,
        policy_bytes,
        &key_commitment,
    )?;
    wrap_dek(new_master, &dek, scope, policy_bytes, &key_commitment)
}

fn commitment(dek: &[u8; 32], scope: FileContextScope, policy: &[u8]) -> [u8; 32] {
    derive(
        "w9pt v3 file DEK commitment",
        &[
            dek,
            &scope.filesystem_id,
            scope.file_id.as_bytes(),
            &scope.owner_inode_id,
            scope.context_id.as_bytes(),
            policy,
        ],
    )
}

fn wrap_aad(
    master_id: MasterKeyId,
    scope: FileContextScope,
    policy: &[u8],
    commitment: &[u8; 32],
) -> Vec<u8> {
    let mut aad = Vec::with_capacity(16 * 4 + policy.len() + 32);
    aad.extend_from_slice(master_id.as_bytes());
    aad.extend_from_slice(&scope.filesystem_id);
    aad.extend_from_slice(&scope.owner_inode_id);
    aad.extend_from_slice(scope.file_id.as_bytes());
    aad.extend_from_slice(scope.context_id.as_bytes());
    aad.extend_from_slice(
        &u16::try_from(policy.len())
            .unwrap_or(u16::MAX)
            .to_le_bytes(),
    );
    aad.extend_from_slice(policy);
    aad.extend_from_slice(commitment);
    aad
}

fn wrap_dek(
    master: &MasterKey,
    dek: &[u8; 32],
    scope: FileContextScope,
    policy: &[u8],
    commitment: &[u8; 32],
) -> Result<Vec<u8>, RepresentationError> {
    let working = Zeroizing::new(derive::<64>(
        "w9pt v3 master KEK wrapping key",
        &[master.bytes(), master.id.as_bytes()],
    ));
    let aad = wrap_aad(master.id, scope, policy, commitment);
    let ciphertext = wrap_encrypt(&working, &aad, dek)?;
    let mut wrapped = Vec::with_capacity(8 + 2 + 16 + 2 + ciphertext.len());
    wrapped.extend_from_slice(&WRAP_MAGIC);
    wrapped.extend_from_slice(&WRAP_VERSION.to_le_bytes());
    wrapped.extend_from_slice(master.id.as_bytes());
    wrapped.extend_from_slice(
        &u16::try_from(ciphertext.len())
            .map_err(|_| RepresentationError::InvalidLength)?
            .to_le_bytes(),
    );
    wrapped.extend_from_slice(&ciphertext);
    Ok(wrapped)
}

fn unwrap_dek(
    master: &MasterKey,
    wrapped: &[u8],
    scope: FileContextScope,
    policy: &[u8],
    expected_commitment: &[u8; 32],
) -> Result<Zeroizing<[u8; 32]>, RepresentationError> {
    if wrapped.len() != 8 + 2 + 16 + 2 + WRAPPED_DEK_BYTES || wrapped[..8] != WRAP_MAGIC {
        return Err(RepresentationError::InvalidWrappedKey);
    }
    if u16::from_le_bytes(wrapped[8..10].try_into().expect("fixed slice")) != WRAP_VERSION {
        return Err(RepresentationError::UnsupportedWrappedKey);
    }
    let master_id = MasterKeyId::new(wrapped[10..26].try_into().expect("fixed slice"));
    if master_id != master.id {
        return Err(RepresentationError::WrongMaster);
    }
    let length = usize::from(u16::from_le_bytes(
        wrapped[26..28].try_into().expect("fixed slice"),
    ));
    if length != WRAPPED_DEK_BYTES || wrapped.len() != 28 + length {
        return Err(RepresentationError::InvalidWrappedKey);
    }
    let working = Zeroizing::new(derive::<64>(
        "w9pt v3 master KEK wrapping key",
        &[master.bytes(), master.id.as_bytes()],
    ));
    let aad = wrap_aad(master.id, scope, policy, expected_commitment);
    let mut plaintext = wrap_decrypt(&working, &aad, &wrapped[28..])?;
    if plaintext.len() != DEK_BYTES {
        plaintext.zeroize();
        return Err(RepresentationError::InvalidWrappedKey);
    }
    let mut dek = Zeroizing::new([0; DEK_BYTES]);
    dek.copy_from_slice(&plaintext);
    plaintext.zeroize();
    let actual = commitment(&dek, scope, policy);
    if !constant_time_eq(&actual, expected_commitment) {
        return Err(RepresentationError::CommitmentMismatch);
    }
    Ok(dek)
}

#[cfg(feature = "encryption-aes-siv")]
fn wrap_encrypt(
    key: &[u8; 64],
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, RepresentationError> {
    use aes_siv::{KeyInit, siv::Aes256Siv};
    let mut cipher = Aes256Siv::new(key.into());
    cipher
        .encrypt([aad], plaintext)
        .map_err(|_| RepresentationError::WrappingFailed)
}

#[cfg(not(feature = "encryption-aes-siv"))]
fn wrap_encrypt(
    _key: &[u8; 64],
    _aad: &[u8],
    _plaintext: &[u8],
) -> Result<Vec<u8>, RepresentationError> {
    Err(RepresentationError::UnsupportedCipher)
}

#[cfg(feature = "encryption-aes-siv")]
fn wrap_decrypt(
    key: &[u8; 64],
    aad: &[u8],
    ciphertext: &[u8],
) -> Result<Vec<u8>, RepresentationError> {
    use aes_siv::{KeyInit, siv::Aes256Siv};
    let mut cipher = Aes256Siv::new(key.into());
    cipher
        .decrypt([aad], ciphertext)
        .map_err(|_| RepresentationError::AuthenticationFailed)
}

#[cfg(not(feature = "encryption-aes-siv"))]
fn wrap_decrypt(
    _key: &[u8; 64],
    _aad: &[u8],
    _ciphertext: &[u8],
) -> Result<Vec<u8>, RepresentationError> {
    Err(RepresentationError::UnsupportedCipher)
}

fn constant_time_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

pub(super) fn implicit_plain_context(file_id: FileId, method: StorageMethod) -> FileCryptoContext {
    let policy = FileStoragePolicy::plain(method);
    let context_id = ContentContextId::new(*file_id.as_bytes());
    let binding = ContentContextBinding::new(
        file_id,
        context_id,
        FileStoragePolicy::FORMAT,
        policy.to_bytes().to_vec(),
        None,
        0,
    )
    .expect("implicit plain binding is valid");
    FileCryptoContext::new(binding, policy, None)
}
