//! Strong identifiers and portable content references.

use core::fmt;

use crate::StorageMethod;

/// Stable, caller-supplied identity of one logical file.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FileId([u8; 16]);

impl FileId {
    /// Creates an identifier from its canonical 16 bytes.
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Creates an identifier from an integer, encoded in big-endian order.
    pub const fn from_u128(value: u128) -> Self {
        Self(value.to_be_bytes())
    }

    /// Returns the canonical bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl fmt::Debug for FileId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "FileId({})", Hex(&self.0))
    }
}

/// Globally stable, caller-supplied identity of one logical mutation.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MutationId([u8; 16]);

impl MutationId {
    /// Creates an identifier from its canonical 16 bytes.
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Creates an identifier from an integer, encoded in big-endian order.
    pub const fn from_u128(value: u128) -> Self {
        Self(value.to_be_bytes())
    }

    /// Returns the canonical bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl fmt::Debug for MutationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "MutationId({})", Hex(&self.0))
    }
}

/// An opaque key interpreted only by a [`crate::TargetStore`] implementation.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ObjectKey(Box<str>);

impl ObjectKey {
    /// Constructs a non-empty key without ASCII control characters.
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidObjectKey> {
        let value = value.into();
        if value.is_empty() {
            return Err(InvalidObjectKey::Empty);
        }
        if value.bytes().any(|byte| byte.is_ascii_control()) {
            return Err(InvalidObjectKey::ControlCharacter);
        }
        Ok(Self(value.into_boxed_str()))
    }

    /// Returns the target-facing key text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn from_validated(value: String) -> Self {
        Self(value.into_boxed_str())
    }
}

impl fmt::Debug for ObjectKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("ObjectKey").field(&self.0).finish()
    }
}

impl fmt::Display for ObjectKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Reason an [`ObjectKey`] could not be constructed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidObjectKey {
    /// Keys must not be empty.
    Empty,
    /// Control characters are forbidden in repository keys.
    ControlCharacter,
}

impl fmt::Display for InvalidObjectKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("object key is empty"),
            Self::ControlCharacter => {
                formatter.write_str("object key contains a control character")
            }
        }
    }
}

impl std::error::Error for InvalidObjectKey {}

/// Opaque target revision returned by exact object reads.
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct ObjectVersion(Box<[u8]>);

impl ObjectVersion {
    /// Wraps target-defined revision bytes.
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into().into_boxed_slice())
    }

    /// Returns the opaque target revision bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for ObjectVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ObjectVersion")
            .field(&Hex(&self.0))
            .finish()
    }
}

/// A 256-bit digest of canonical plaintext or encoded manifest bytes.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Digest([u8; 32]);

impl Digest {
    /// Creates a digest from its canonical bytes.
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the canonical bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Computes the version-1 BLAKE3-256 digest of canonical plaintext bytes.
    pub fn blake3(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Digest({})", Hex(&self.0))
    }
}

/// Portable reference to one immutable, fully prepared file-content version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContentRef {
    file_id: FileId,
    generation: u64,
    logical_size: u64,
    manifest: ObjectKey,
    manifest_digest: Digest,
    method: StorageMethod,
}

impl ContentRef {
    /// Reconstructs a structurally valid reference read from authoritative metadata.
    ///
    /// Call [`crate::ContentRepository::validate_content`] before treating target
    /// data as usable. That validation binds these summary fields and digest to the
    /// immutable manifest bytes.
    pub fn from_persisted(
        file_id: FileId,
        generation: u64,
        logical_size: u64,
        manifest: ObjectKey,
        manifest_digest: Digest,
        method: StorageMethod,
    ) -> Result<Self, InvalidContentRef> {
        if generation == 0 {
            return Err(InvalidContentRef::ZeroGeneration);
        }
        Ok(Self {
            file_id,
            generation,
            logical_size,
            manifest,
            manifest_digest,
            method,
        })
    }

    pub(crate) fn new_validated(
        file_id: FileId,
        generation: u64,
        logical_size: u64,
        manifest: ObjectKey,
        manifest_digest: Digest,
        method: StorageMethod,
    ) -> Self {
        debug_assert_ne!(generation, 0);
        Self {
            file_id,
            generation,
            logical_size,
            manifest,
            manifest_digest,
            method,
        }
    }

    /// Returns the owning file identity.
    pub const fn file_id(&self) -> FileId {
        self.file_id
    }

    /// Returns the monotonically increasing content generation.
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Returns authoritative logical EOF.
    pub const fn logical_size(&self) -> u64 {
        self.logical_size
    }

    /// Returns the immutable manifest key.
    pub fn manifest_key(&self) -> &ObjectKey {
        &self.manifest
    }

    /// Returns the digest of the encoded manifest object.
    pub const fn manifest_digest(&self) -> Digest {
        self.manifest_digest
    }

    /// Returns the persisted layout method.
    pub const fn method(&self) -> StorageMethod {
        self.method
    }
}

/// Structurally invalid persisted [`ContentRef`] fields.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidContentRef {
    /// Content generations start at one.
    ZeroGeneration,
}

impl fmt::Display for InvalidContentRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroGeneration => formatter.write_str("content generation must be nonzero"),
        }
    }
}

impl std::error::Error for InvalidContentRef {}

/// Immutable identity of the content version against which an operation was prepared.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct BaseContentIdentity {
    generation: u64,
    manifest_digest: Digest,
}

impl BaseContentIdentity {
    /// Distinguished base used only while creating a new file.
    pub const NEW_FILE: Self = Self {
        generation: 0,
        manifest_digest: Digest::new([0; 32]),
    };

    /// Captures the identity of an existing immutable content version.
    pub const fn from_content(content: &ContentRef) -> Self {
        Self {
            generation: content.generation,
            manifest_digest: content.manifest_digest,
        }
    }

    /// Reconstructs a persisted base identity.
    pub const fn new(generation: u64, manifest_digest: Digest) -> Self {
        Self {
            generation,
            manifest_digest,
        }
    }

    /// Returns the base generation, or zero for new-file creation.
    pub const fn generation(self) -> u64 {
        self.generation
    }

    /// Returns the base manifest digest, or zeroes for new-file creation.
    pub const fn manifest_digest(self) -> Digest {
        self.manifest_digest
    }

    /// Reports whether this is the distinguished new-file base.
    pub fn is_new_file(self) -> bool {
        self.generation == 0 && self.manifest_digest.0 == [0; 32]
    }
}

/// Deterministic digest of one complete logical create, write, or truncate request.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct OperationFingerprint(Digest);

impl OperationFingerprint {
    /// Reconstructs a persisted fingerprint.
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(Digest::new(bytes))
    }

    /// Returns the canonical fingerprint bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }
}

impl fmt::Debug for OperationFingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "OperationFingerprint({})", Hex(self.as_bytes()))
    }
}

/// Collision-safe identity bound into every immutable preparation key.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PreparationIdentity {
    mutation_id: MutationId,
    base: BaseContentIdentity,
    fingerprint: OperationFingerprint,
}

impl PreparationIdentity {
    /// Reconstructs a persisted preparation identity.
    pub const fn new(
        mutation_id: MutationId,
        base: BaseContentIdentity,
        fingerprint: OperationFingerprint,
    ) -> Self {
        Self {
            mutation_id,
            base,
            fingerprint,
        }
    }

    /// Derives the identity of a new-file creation request.
    pub fn for_create(
        mutation_id: MutationId,
        method: StorageMethod,
        bytes: &[u8],
    ) -> Result<Self, crate::RangeError> {
        let length = u64::try_from(bytes.len()).map_err(|_| crate::RangeError::LengthConversion)?;
        let method_tag = match method {
            StorageMethod::Raw => 1,
            StorageMethod::BlockSplit => 2,
        };
        Ok(Self::new(
            mutation_id,
            BaseContentIdentity::NEW_FILE,
            fingerprint(&[b"create", &[method_tag], &length.to_le_bytes(), bytes]),
        ))
    }

    /// Derives the identity of a positioned-write request against `base`.
    pub fn for_write(
        mutation_id: MutationId,
        base: &ContentRef,
        offset: u64,
        bytes: &[u8],
    ) -> Result<Self, crate::RangeError> {
        let length = u64::try_from(bytes.len()).map_err(|_| crate::RangeError::LengthConversion)?;
        Ok(Self::new(
            mutation_id,
            BaseContentIdentity::from_content(base),
            fingerprint(&[
                b"write",
                &offset.to_le_bytes(),
                &length.to_le_bytes(),
                bytes,
            ]),
        ))
    }

    /// Derives the identity of a truncate request against `base`.
    pub fn for_truncate(mutation_id: MutationId, base: &ContentRef, logical_size: u64) -> Self {
        Self::new(
            mutation_id,
            BaseContentIdentity::from_content(base),
            fingerprint(&[b"truncate", &logical_size.to_le_bytes()]),
        )
    }

    /// Returns the caller-supplied logical mutation identity.
    pub const fn mutation_id(self) -> MutationId {
        self.mutation_id
    }

    /// Returns the immutable base content identity.
    pub const fn base(self) -> BaseContentIdentity {
        self.base
    }

    /// Returns the deterministic logical-operation fingerprint.
    pub const fn fingerprint(self) -> OperationFingerprint {
        self.fingerprint
    }
}

fn fingerprint(parts: &[&[u8]]) -> OperationFingerprint {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"w9pt-storage-operation-v1\0");
    for part in parts {
        let length = u64::try_from(part.len()).unwrap_or(u64::MAX);
        hasher.update(&length.to_le_bytes());
        hasher.update(part);
    }
    OperationFingerprint::new(*hasher.finalize().as_bytes())
}

/// Result of preparing immutable content before authoritative publication.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedContent {
    content: ContentRef,
    content_changed: bool,
    identity: PreparationIdentity,
    attempt: u32,
}

impl PreparedContent {
    /// Constructs a preparation result.
    pub(crate) const fn new(
        content: ContentRef,
        content_changed: bool,
        identity: PreparationIdentity,
        attempt: u32,
    ) -> Self {
        Self {
            content,
            content_changed,
            identity,
            attempt,
        }
    }

    /// Returns the prepared portable reference.
    pub const fn content(&self) -> &ContentRef {
        &self.content
    }

    /// Consumes the result and returns its portable reference.
    pub fn into_content(self) -> ContentRef {
        self.content
    }

    /// Reports whether logical content changed from the base version.
    pub const fn content_changed(&self) -> bool {
        self.content_changed
    }

    /// Returns the base- and fingerprint-bound preparation identity.
    pub const fn identity(&self) -> PreparationIdentity {
        self.identity
    }

    /// Returns the attempt component used in immutable keys.
    pub const fn attempt(&self) -> u32 {
        self.attempt
    }
}

struct Hex<'a>(&'a [u8]);

impl fmt::Display for Hex<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Hex<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_have_fixed_width_debug_encodings() {
        assert_eq!(
            format!("{:?}", FileId::from_u128(1)),
            "FileId(00000000000000000000000000000001)"
        );
        assert_eq!(
            format!("{:?}", MutationId::from_u128(2)),
            "MutationId(00000000000000000000000000000002)"
        );
    }

    #[test]
    fn object_keys_reject_unsafe_text() {
        assert_eq!(ObjectKey::new(""), Err(InvalidObjectKey::Empty));
        assert_eq!(
            ObjectKey::new("a\nb"),
            Err(InvalidObjectKey::ControlCharacter)
        );
        assert_eq!(ObjectKey::new("v1/data").unwrap().as_str(), "v1/data");
    }

    #[test]
    fn persisted_content_reference_rejects_zero_generation() {
        assert_eq!(
            ContentRef::from_persisted(
                FileId::from_u128(1),
                0,
                0,
                ObjectKey::new("manifest").unwrap(),
                Digest::new([0; 32]),
                StorageMethod::Raw,
            ),
            Err(InvalidContentRef::ZeroGeneration)
        );
    }
}
