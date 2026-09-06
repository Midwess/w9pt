//! Storage-method configuration.

use core::fmt;

/// Canonical logical block size for the current block-split format.
pub const BLOCK_SIZE: u32 = 32 * 1024;
/// Number of slots in every current mapping page.
pub const BLOCK_MAP_FANOUT: u16 = 128;
/// Number of block-index bits selected by each mapping-page level.
pub const BLOCK_MAP_INDEX_BITS: u8 = 7;
/// Highest supported mapping-page level (leaf level is zero).
pub const BLOCK_MAP_MAX_LEVEL: u8 = 6;

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

/// Current plaintext digest algorithm.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum HashAlgorithm {
    /// BLAKE3 with a 256-bit output.
    Blake3_256,
}

/// Current payload codec.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PayloadCodec {
    /// Stored bytes are the canonical plaintext bytes.
    Identity,
}

/// Current payload encryption mode.
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
    /// The only representation supported by the current format.
    pub const CURRENT: Self = Self {
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

/// Persisted parameters for the current paged block-split format.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct BlockSplitParameters {
    block_size: u32,
    fanout: u16,
    index_bits: u8,
    max_level: u8,
}

impl BlockSplitParameters {
    /// The canonical current parameters.
    pub const CURRENT: Self = Self {
        block_size: BLOCK_SIZE,
        fanout: BLOCK_MAP_FANOUT,
        index_bits: BLOCK_MAP_INDEX_BITS,
        max_level: BLOCK_MAP_MAX_LEVEL,
    };

    /// Validates geometry decoded from persistent storage.
    pub const fn from_persisted(
        block_size: u32,
        fanout: u16,
        index_bits: u8,
        max_level: u8,
    ) -> Result<Self, UnsupportedBlockProfile> {
        if block_size == BLOCK_SIZE
            && fanout == BLOCK_MAP_FANOUT
            && index_bits == BLOCK_MAP_INDEX_BITS
            && max_level == BLOCK_MAP_MAX_LEVEL
        {
            Ok(Self::CURRENT)
        } else {
            Err(UnsupportedBlockProfile {
                block_size,
                fanout,
                index_bits,
                max_level,
            })
        }
    }

    /// Returns the logical plaintext block size.
    pub const fn block_size(self) -> u32 {
        self.block_size
    }

    /// Returns the slots available in every leaf or branch page.
    pub const fn fanout(self) -> u16 {
        self.fanout
    }

    /// Returns the block-index bits consumed by one page level.
    pub const fn index_bits(self) -> u8 {
        self.index_bits
    }

    /// Returns the highest supported branch-page level.
    pub const fn max_level(self) -> u8 {
        self.max_level
    }
}

/// Unsupported persisted block-split geometry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnsupportedBlockProfile {
    block_size: u32,
    fanout: u16,
    index_bits: u8,
    max_level: u8,
}

impl UnsupportedBlockProfile {
    /// Returns the rejected block size.
    pub const fn block_size(self) -> u32 {
        self.block_size
    }

    /// Returns the rejected fanout.
    pub const fn fanout(self) -> u16 {
        self.fanout
    }
}

impl fmt::Display for UnsupportedBlockProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "unsupported block profile ({}, {}, {}, {}); expected ({BLOCK_SIZE}, {BLOCK_MAP_FANOUT}, {BLOCK_MAP_INDEX_BITS}, {BLOCK_MAP_MAX_LEVEL})",
            self.block_size, self.fanout, self.index_bits, self.max_level
        )
    }
}

impl std::error::Error for UnsupportedBlockProfile {}

/// Validated defaults used only when creating a new file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CreationDefaults {
    method: StorageMethod,
    representation: Representation,
    block_split: BlockSplitParameters,
}

impl CreationDefaults {
    /// Creates supported current-format defaults for newly created files.
    pub const fn new(method: StorageMethod) -> Self {
        Self {
            method,
            representation: Representation::CURRENT,
            block_split: BlockSplitParameters::CURRENT,
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
    fn persisted_block_profile_is_fixed() {
        assert_eq!(
            BlockSplitParameters::from_persisted(
                BLOCK_SIZE,
                BLOCK_MAP_FANOUT,
                BLOCK_MAP_INDEX_BITS,
                BLOCK_MAP_MAX_LEVEL,
            ),
            Ok(BlockSplitParameters::CURRENT)
        );
        assert_eq!(
            BlockSplitParameters::from_persisted(
                BLOCK_SIZE / 2,
                BLOCK_MAP_FANOUT,
                BLOCK_MAP_INDEX_BITS,
                BLOCK_MAP_MAX_LEVEL,
            )
            .unwrap_err()
            .block_size(),
            BLOCK_SIZE / 2
        );
    }

    #[test]
    fn creation_defaults_are_supported_and_explicit() {
        let defaults = CreationDefaults::default();
        assert_eq!(defaults.method(), StorageMethod::BlockSplit);
        assert_eq!(defaults.representation(), Representation::CURRENT);
        assert_eq!(defaults.block_split().block_size(), BLOCK_SIZE);
        assert_eq!(StorageMethod::Raw.config_name(), "raw");
    }
}
