//! Storage-method configuration.

use core::fmt;

/// Canonical logical block size for block-split format version 1.
pub const BLOCK_SIZE_V1: u32 = 32 * 1024;

/// Persisted layout used for one logical file version.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum StorageMethod {
    /// Store the complete logical file in one immutable payload object.
    Raw,
    /// Store sparse, fixed-size logical blocks in separate immutable objects.
    BlockSplit,
}

impl StorageMethod {
    /// Returns the stable configuration name.
    pub const fn config_name(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::BlockSplit => "block-split",
        }
    }
}

/// Version-1 plaintext digest algorithm.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum HashAlgorithm {
    /// BLAKE3 with a 256-bit output.
    Blake3_256,
}

/// Version-1 payload codec.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PayloadCodec {
    /// Stored bytes are the canonical plaintext bytes.
    Identity,
}

/// Version-1 payload encryption mode.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PayloadCipher {
    /// Stored bytes are not encrypted.
    None,
}

/// Persisted payload representation, independent of file layout.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Representation {
    hash: HashAlgorithm,
    codec: PayloadCodec,
    cipher: PayloadCipher,
}

impl Representation {
    /// The only representation supported by format version 1.
    pub const V1: Self = Self {
        hash: HashAlgorithm::Blake3_256,
        codec: PayloadCodec::Identity,
        cipher: PayloadCipher::None,
    };

    /// Returns the plaintext hash algorithm.
    pub const fn hash(self) -> HashAlgorithm {
        self.hash
    }

    /// Returns the stored payload codec.
    pub const fn codec(self) -> PayloadCodec {
        self.codec
    }

    /// Returns the stored payload cipher.
    pub const fn cipher(self) -> PayloadCipher {
        self.cipher
    }
}

/// Persisted parameters for block-split format version 1.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct BlockSplitParameters {
    block_size: u32,
}

impl BlockSplitParameters {
    /// The canonical version-1 parameters.
    pub const V1: Self = Self {
        block_size: BLOCK_SIZE_V1,
    };

    /// Validates a block size decoded from persistent storage.
    pub const fn from_persisted(block_size: u32) -> Result<Self, UnsupportedBlockSize> {
        if block_size == BLOCK_SIZE_V1 {
            Ok(Self::V1)
        } else {
            Err(UnsupportedBlockSize { block_size })
        }
    }

    /// Returns the logical plaintext block size.
    pub const fn block_size(self) -> u32 {
        self.block_size
    }
}

/// Unsupported persisted block-split parameter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnsupportedBlockSize {
    block_size: u32,
}

impl UnsupportedBlockSize {
    /// Returns the rejected block size.
    pub const fn block_size(self) -> u32 {
        self.block_size
    }
}

impl fmt::Display for UnsupportedBlockSize {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "unsupported block size {}; expected {BLOCK_SIZE_V1}",
            self.block_size
        )
    }
}

impl std::error::Error for UnsupportedBlockSize {}

/// Validated defaults used only when creating a new file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CreationDefaults {
    method: StorageMethod,
    representation: Representation,
    block_split: BlockSplitParameters,
}

impl CreationDefaults {
    /// Creates supported version-1 defaults for newly created files.
    pub const fn new(method: StorageMethod) -> Self {
        Self {
            method,
            representation: Representation::V1,
            block_split: BlockSplitParameters::V1,
        }
    }

    /// Returns the default method for new files.
    pub const fn method(self) -> StorageMethod {
        self.method
    }

    /// Returns the supported representation.
    pub const fn representation(self) -> Representation {
        self.representation
    }

    /// Returns the block-split parameters persisted for new block-split files.
    pub const fn block_split(self) -> BlockSplitParameters {
        self.block_split
    }
}

impl Default for CreationDefaults {
    fn default() -> Self {
        Self::new(StorageMethod::BlockSplit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_block_size_is_fixed_for_v1() {
        assert_eq!(
            BlockSplitParameters::from_persisted(BLOCK_SIZE_V1),
            Ok(BlockSplitParameters::V1)
        );
        assert_eq!(
            BlockSplitParameters::from_persisted(BLOCK_SIZE_V1 / 2)
                .unwrap_err()
                .block_size(),
            BLOCK_SIZE_V1 / 2
        );
    }

    #[test]
    fn creation_defaults_are_supported_and_explicit() {
        let defaults = CreationDefaults::default();
        assert_eq!(defaults.method(), StorageMethod::BlockSplit);
        assert_eq!(defaults.representation(), Representation::V1);
        assert_eq!(defaults.block_split().block_size(), BLOCK_SIZE_V1);
        assert_eq!(StorageMethod::Raw.config_name(), "raw");
    }
}
