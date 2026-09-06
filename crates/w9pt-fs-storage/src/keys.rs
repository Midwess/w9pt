//! Canonical private-prefix target key construction.

use crate::{
    BaseContentIdentity, ConfigurationError, CorruptionError, Digest, FileId, MutationId,
    ObjectKey, OperationFingerprint, PreparationIdentity, StorageLimits,
};

const BLOCK_PAYLOAD_SUFFIX_BYTES: usize =
    "/v3/data/".len() + 32 + 1 + 32 + 1 + 16 + 1 + 64 + 1 + 64 + 1 + 8 + "/blocks/".len() + 16;
const MAP_PAGE_SUFFIX_BYTES: usize =
    "/v3/maps/".len() + 32 + 1 + 32 + 1 + 16 + 1 + 64 + 1 + 64 + 1 + 8 + 1 + 2 + 1 + 16;
const LONGEST_SUFFIX_BYTES: usize = if BLOCK_PAYLOAD_SUFFIX_BYTES > MAP_PAGE_SUFFIX_BYTES {
    BLOCK_PAYLOAD_SUFFIX_BYTES
} else {
    MAP_PAGE_SUFFIX_BYTES
};

/// Validated private object keyspace for one repository.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeySpace {
    prefix: Box<str>,
    max_key_bytes: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PreparationKey {
    pub(crate) identity: PreparationIdentity,
    pub(crate) attempt: u32,
}

impl PreparationKey {
    pub(crate) fn validate_result_generation(self, generation: u64) -> Result<(), CorruptionError> {
        let base = self.identity.base();
        let valid = if generation == 1 {
            base.is_new_file()
        } else {
            !base.is_new_file() && base.generation().checked_add(1) == Some(generation)
        };
        if valid {
            Ok(())
        } else {
            Err(CorruptionError::IdentityMismatch {
                field: "base generation",
            })
        }
    }
}

impl KeySpace {
    pub(crate) fn ensure_owned(&self, key: &ObjectKey) -> Result<(), CorruptionError> {
        self.components(key).map(|_| ())
    }
    /// Validates a private prefix and its longest current-format key.
    pub fn new(
        prefix: impl Into<String>,
        limits: StorageLimits,
    ) -> Result<Self, ConfigurationError> {
        let prefix = prefix.into();
        if prefix.is_empty() {
            return Err(ConfigurationError::InvalidPrefix {
                reason: "prefix is empty",
            });
        }
        if prefix.starts_with('/') || prefix.ends_with('/') {
            return Err(ConfigurationError::InvalidPrefix {
                reason: "leading or trailing slash",
            });
        }
        if prefix.bytes().any(|byte| byte.is_ascii_control()) {
            return Err(ConfigurationError::InvalidPrefix {
                reason: "control character",
            });
        }
        if prefix
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
        {
            return Err(ConfigurationError::InvalidPrefix {
                reason: "empty or relative component",
            });
        }
        let longest = prefix.len().checked_add(LONGEST_SUFFIX_BYTES).ok_or(
            ConfigurationError::InvalidPrefix {
                reason: "key length overflow",
            },
        )?;
        if longest > limits.max_key_bytes() {
            return Err(ConfigurationError::InvalidPrefix {
                reason: "current-format key exceeds max_key_bytes",
            });
        }
        Ok(Self {
            prefix: prefix.into_boxed_str(),
            max_key_bytes: limits.max_key_bytes(),
        })
    }

    /// Returns the canonical repository format marker key.
    pub fn format(&self) -> ObjectKey {
        self.key(format_args!("{}/v3/format", self.prefix))
    }

    /// Returns the sole mutable publication key for a file.
    pub fn head(&self, file_id: FileId) -> ObjectKey {
        self.key(format_args!(
            "{}/v3/refs/files/{}",
            self.prefix,
            IdHex(file_id.as_bytes())
        ))
    }

    /// Returns an attempt-specific immutable manifest key.
    pub fn manifest(
        &self,
        file_id: FileId,
        identity: PreparationIdentity,
        attempt: u32,
    ) -> ObjectKey {
        self.key(format_args!(
            "{}/v3/manifests/{}/{}/{:016x}/{}/{}/{attempt:08x}",
            self.prefix,
            IdHex(file_id.as_bytes()),
            IdHex(identity.mutation_id().as_bytes()),
            identity.base().generation(),
            IdHex(identity.base().manifest_digest().as_bytes()),
            IdHex(identity.fingerprint().as_bytes()),
        ))
    }

    /// Returns an attempt-specific immutable raw payload key.
    pub fn raw_payload(
        &self,
        file_id: FileId,
        identity: PreparationIdentity,
        attempt: u32,
    ) -> ObjectKey {
        self.key(format_args!(
            "{}/v3/data/{}/{}/{:016x}/{}/{}/{attempt:08x}/raw",
            self.prefix,
            IdHex(file_id.as_bytes()),
            IdHex(identity.mutation_id().as_bytes()),
            identity.base().generation(),
            IdHex(identity.base().manifest_digest().as_bytes()),
            IdHex(identity.fingerprint().as_bytes()),
        ))
    }

    /// Returns an attempt- and block-specific immutable payload key.
    pub fn block_payload(
        &self,
        file_id: FileId,
        identity: PreparationIdentity,
        attempt: u32,
        block_index: u64,
    ) -> ObjectKey {
        self.key(format_args!(
            "{}/v3/data/{}/{}/{:016x}/{}/{}/{attempt:08x}/blocks/{block_index:016x}",
            self.prefix,
            IdHex(file_id.as_bytes()),
            IdHex(identity.mutation_id().as_bytes()),
            identity.base().generation(),
            IdHex(identity.base().manifest_digest().as_bytes()),
            IdHex(identity.fingerprint().as_bytes()),
        ))
    }

    /// Returns an attempt- and location-specific immutable mapping-page key.
    pub fn map_page(
        &self,
        file_id: FileId,
        identity: PreparationIdentity,
        attempt: u32,
        level: u8,
        first_block: u64,
    ) -> ObjectKey {
        self.key(format_args!(
            "{}/v3/maps/{}/{}/{:016x}/{}/{}/{attempt:08x}/{level:02x}/{first_block:016x}",
            self.prefix,
            IdHex(file_id.as_bytes()),
            IdHex(identity.mutation_id().as_bytes()),
            identity.base().generation(),
            IdHex(identity.base().manifest_digest().as_bytes()),
            IdHex(identity.fingerprint().as_bytes()),
        ))
    }

    /// Returns a protected-token immutable manifest key with no public fingerprint.
    pub fn protected_manifest(&self, file_id: FileId, token: [u8; 32]) -> ObjectKey {
        self.key(format_args!(
            "{}/v3/protected/{}/{}/manifest",
            self.prefix,
            IdHex(file_id.as_bytes()),
            IdHex(&token),
        ))
    }

    /// Returns a protected-token raw payload key.
    pub fn protected_raw_payload(&self, file_id: FileId, token: [u8; 32]) -> ObjectKey {
        self.key(format_args!(
            "{}/v3/protected/{}/{}/raw",
            self.prefix,
            IdHex(file_id.as_bytes()),
            IdHex(&token),
        ))
    }

    /// Returns a protected-token block payload key.
    pub fn protected_block_payload(
        &self,
        file_id: FileId,
        token: [u8; 32],
        block_index: u64,
    ) -> ObjectKey {
        self.key(format_args!(
            "{}/v3/protected/{}/{}/blocks/{block_index:016x}",
            self.prefix,
            IdHex(file_id.as_bytes()),
            IdHex(&token),
        ))
    }

    /// Returns a protected-token mapping page key.
    pub fn protected_map_page(
        &self,
        file_id: FileId,
        token: [u8; 32],
        level: u8,
        first_block: u64,
    ) -> ObjectKey {
        self.key(format_args!(
            "{}/v3/protected/{}/{}/maps/{level:02x}/{first_block:016x}",
            self.prefix,
            IdHex(file_id.as_bytes()),
            IdHex(&token),
        ))
    }

    pub(crate) fn parse_manifest(
        &self,
        expected_file: FileId,
        key: &ObjectKey,
    ) -> Result<PreparationKey, CorruptionError> {
        let components = self.components(key)?;
        if components.len() != 8 || components[0] != "v3" || components[1] != "manifests" {
            return Err(CorruptionError::InvalidKeySchema);
        }
        parse_preparation_components(expected_file, &components[2..])
    }

    pub(crate) fn validate_raw_payload(
        &self,
        file_id: FileId,
        key: &ObjectKey,
    ) -> Result<(), CorruptionError> {
        let components = self.components(key)?;
        if components.len() != 9
            || components[0] != "v3"
            || components[1] != "data"
            || components[8] != "raw"
        {
            return Err(CorruptionError::InvalidKeySchema);
        }
        parse_preparation_components(file_id, &components[2..8]).map(|_| ())
    }

    pub(crate) fn validate_block_payload(
        &self,
        file_id: FileId,
        block_index: u64,
        key: &ObjectKey,
    ) -> Result<(), CorruptionError> {
        let components = self.components(key)?;
        if components.len() != 10
            || components[0] != "v3"
            || components[1] != "data"
            || components[8] != "blocks"
            || parse_fixed_hex_u64(components[9], 16)? != block_index
        {
            return Err(CorruptionError::InvalidKeySchema);
        }
        parse_preparation_components(file_id, &components[2..8]).map(|_| ())
    }

    pub(crate) fn validate_map_page(
        &self,
        file_id: FileId,
        level: u8,
        first_block: u64,
        key: &ObjectKey,
    ) -> Result<(), CorruptionError> {
        let components = self.components(key)?;
        if components.len() != 10
            || components[0] != "v3"
            || components[1] != "maps"
            || parse_fixed_hex_u64(components[8], 2)? != u64::from(level)
            || parse_fixed_hex_u64(components[9], 16)? != first_block
        {
            return Err(CorruptionError::InvalidKeySchema);
        }
        parse_preparation_components(file_id, &components[2..8]).map(|_| ())
    }

    fn components<'a>(&self, key: &'a ObjectKey) -> Result<Vec<&'a str>, CorruptionError> {
        if key.as_str().len() > self.max_key_bytes {
            return Err(CorruptionError::InvalidKeySchema);
        }
        let suffix = key
            .as_str()
            .strip_prefix(self.prefix.as_ref())
            .and_then(|suffix| suffix.strip_prefix('/'))
            .ok_or(CorruptionError::ForeignKey)?;
        Ok(suffix.split('/').collect())
    }

    fn key(&self, arguments: core::fmt::Arguments<'_>) -> ObjectKey {
        ObjectKey::from_validated(arguments.to_string())
    }
}

fn parse_preparation_components(
    expected_file: FileId,
    components: &[&str],
) -> Result<PreparationKey, CorruptionError> {
    if components.len() != 6 {
        return Err(CorruptionError::InvalidKeySchema);
    }
    let file_id = FileId::new(parse_hex_array(components[0])?);
    if file_id != expected_file {
        return Err(CorruptionError::IdentityMismatch { field: "file" });
    }
    let mutation_id = MutationId::new(parse_hex_array(components[1])?);
    let base_generation = parse_fixed_hex_u64(components[2], 16)?;
    let base_digest = Digest::new(parse_hex_array(components[3])?);
    let fingerprint = OperationFingerprint::new(parse_hex_array(components[4])?);
    let attempt = parse_fixed_hex_u32(components[5], 8)?;
    Ok(PreparationKey {
        identity: PreparationIdentity::new(
            mutation_id,
            BaseContentIdentity::new(base_generation, base_digest),
            fingerprint,
        ),
        attempt,
    })
}

fn parse_hex_array<const N: usize>(value: &str) -> Result<[u8; N], CorruptionError> {
    if value.len() != N * 2 {
        return Err(CorruptionError::InvalidKeySchema);
    }
    let mut output = [0; N];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        output[index] = (hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?;
    }
    Ok(output)
}

fn parse_fixed_hex_u64(value: &str, width: usize) -> Result<u64, CorruptionError> {
    if value.len() != width || !value.bytes().all(is_lower_hex) {
        return Err(CorruptionError::InvalidKeySchema);
    }
    u64::from_str_radix(value, 16).map_err(|_| CorruptionError::InvalidKeySchema)
}

fn parse_fixed_hex_u32(value: &str, width: usize) -> Result<u32, CorruptionError> {
    if value.len() != width || !value.bytes().all(is_lower_hex) {
        return Err(CorruptionError::InvalidKeySchema);
    }
    u32::from_str_radix(value, 16).map_err(|_| CorruptionError::InvalidKeySchema)
}

fn hex_nibble(value: u8) -> Result<u8, CorruptionError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(CorruptionError::InvalidKeySchema),
    }
}

fn is_lower_hex(value: u8) -> bool {
    value.is_ascii_digit() || (b'a'..=b'f').contains(&value)
}

struct IdHex<'a>(&'a [u8]);

impl core::fmt::Display for IdHex<'_> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BaseContentIdentity, ContentRef, Digest, MutationId, OperationFingerprint,
        PreparationIdentity, StorageMethod,
    };

    fn identity() -> PreparationIdentity {
        PreparationIdentity::new(
            MutationId::from_u128(2),
            BaseContentIdentity::NEW_FILE,
            OperationFingerprint::new([3; 32]),
        )
    }

    #[test]
    fn keys_are_private_fixed_width_and_path_free() {
        let keys = KeySpace::new("private/root", StorageLimits::default()).unwrap();
        assert_eq!(
            keys.head(FileId::from_u128(1)).as_str(),
            "private/root/v3/refs/files/00000000000000000000000000000001"
        );
        assert_eq!(
            keys.block_payload(FileId::from_u128(1), identity(), 3, 4)
                .as_str(),
            concat!(
                "private/root/v3/data/00000000000000000000000000000001/",
                "00000000000000000000000000000002/0000000000000000/",
                "0000000000000000000000000000000000000000000000000000000000000000/",
                "0303030303030303030303030303030303030303030303030303030303030303/",
                "00000003/blocks/0000000000000004"
            )
        );
        assert!(
            keys.map_page(FileId::from_u128(1), identity(), 3, 2, 16_384)
                .as_str()
                .ends_with("/00000003/02/0000000000004000")
        );
    }

    #[test]
    fn noncanonical_and_oversized_prefixes_are_rejected() {
        for prefix in [
            "",
            "/private",
            "private/",
            "private//root",
            "private/../root",
        ] {
            assert!(KeySpace::new(prefix, StorageLimits::default()).is_err());
        }
        let values = crate::StorageLimitValues {
            max_key_bytes: 64,
            ..crate::StorageLimitValues::default()
        };
        assert!(KeySpace::new("private", StorageLimits::new(values).unwrap()).is_err());

        let limits = StorageLimits::default();
        let exact = "p".repeat(limits.max_key_bytes() - BLOCK_PAYLOAD_SUFFIX_BYTES);
        let exact_keys = KeySpace::new(exact, limits).unwrap();
        assert_eq!(
            exact_keys
                .block_payload(FileId::from_u128(1), identity(), 0, u64::MAX)
                .as_str()
                .len(),
            limits.max_key_bytes()
        );
        let too_long = "p".repeat(limits.max_key_bytes() - MAP_PAGE_SUFFIX_BYTES);
        assert!(KeySpace::new(too_long, limits).is_err());
    }

    #[test]
    fn preparation_keys_bind_base_and_complete_operation() {
        let keys = KeySpace::new("private", StorageLimits::default()).unwrap();
        let file_id = FileId::from_u128(1);
        let mutation_id = MutationId::from_u128(2);
        let base = ContentRef::from_persisted(
            file_id,
            7,
            3,
            ObjectKey::new("private/v2/manifests/base").unwrap(),
            Digest::new([4; 32]),
            StorageMethod::Raw,
        )
        .unwrap();
        let same = PreparationIdentity::for_write(mutation_id, &base, 9, b"abc").unwrap();
        let same_retry = PreparationIdentity::for_write(mutation_id, &base, 9, b"abc").unwrap();
        let other_request = PreparationIdentity::for_write(mutation_id, &base, 9, b"abd").unwrap();
        let other_base = ContentRef::from_persisted(
            file_id,
            8,
            3,
            ObjectKey::new("private/v2/manifests/other").unwrap(),
            Digest::new([5; 32]),
            StorageMethod::Raw,
        )
        .unwrap();
        let rebased = PreparationIdentity::for_write(mutation_id, &other_base, 9, b"abc").unwrap();

        assert_eq!(
            keys.manifest(file_id, same, 0),
            keys.manifest(file_id, same_retry, 0)
        );
        assert_ne!(
            keys.manifest(file_id, same, 0),
            keys.manifest(file_id, other_request, 0)
        );
        assert_ne!(
            keys.manifest(file_id, same, 0),
            keys.manifest(file_id, rebased, 0)
        );
    }

    #[test]
    fn canonical_preparation_keys_parse_and_validate() {
        let keys = KeySpace::new("private", StorageLimits::default()).unwrap();
        let file_id = FileId::from_u128(1);
        let identity = identity();
        let manifest = keys.manifest(file_id, identity, 3);
        let parsed = keys.parse_manifest(file_id, &manifest).unwrap();
        assert_eq!(parsed.identity, identity);
        assert_eq!(parsed.attempt, 3);
        keys.validate_raw_payload(file_id, &keys.raw_payload(file_id, identity, 3))
            .unwrap();
        keys.validate_block_payload(file_id, 7, &keys.block_payload(file_id, identity, 3, 7))
            .unwrap();
        keys.validate_map_page(
            file_id,
            2,
            16_384,
            &keys.map_page(file_id, identity, 3, 2, 16_384),
        )
        .unwrap();
        assert_eq!(
            keys.parse_manifest(
                file_id,
                &ObjectKey::new("private/v1/manifests/old-development-data").unwrap(),
            ),
            Err(CorruptionError::InvalidKeySchema)
        );
    }
}
