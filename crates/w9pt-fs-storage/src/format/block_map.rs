//! Canonical immutable sparse block-map pages and authenticated references.

use crate::{
    BLOCK_MAP_FANOUT, BLOCK_MAP_MAX_LEVEL, BlockSplitParameters, CorruptionError, Digest, FileId,
    FormatError, LimitError, LimitKind, ObjectKey, StorageLimits,
};

use super::{
    BlobRef, ObjectKind, PersistentFormatError, Reader, Writer, decode_envelope, encode_envelope,
    envelope::checked_envelope_len,
    head::{checked_key_length, read_key, write_key},
    manifest::{blob_encoded_len, read_blob, validate_blob, write_blob},
};

const PAGE_HEADER_BYTES: usize = 16 + 1 + 8 + 4 + 2 + 1 + 1 + 2;
const MINIMUM_BLOB_REF_BYTES: usize = 4 + 1 + 8 + 8 + 3 + 32;
const MINIMUM_PAGE_REF_BYTES: usize = 4 + 1 + 8 + 32 + 1 + 8 + 8 + 8;
const MINIMUM_LEAF_PAGE_BYTES: usize =
    super::envelope::HEADER_LEN + PAGE_HEADER_BYTES + 1 + MINIMUM_BLOB_REF_BYTES;
const MINIMUM_BRANCH_PAGE_BYTES: usize =
    super::envelope::HEADER_LEN + PAGE_HEADER_BYTES + 1 + MINIMUM_PAGE_REF_BYTES;

/// Authenticated reference to one immutable mapping page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PageRef {
    key: ObjectKey,
    encoded_len: u64,
    digest: Digest,
    level: u8,
    first_block: u64,
    materialized_block_count: u64,
    highest_materialized_block: u64,
}

impl PageRef {
    /// Constructs a page reference whose relationships are checked by its enclosing object.
    pub fn new(
        key: ObjectKey,
        encoded_len: u64,
        digest: Digest,
        level: u8,
        first_block: u64,
        materialized_block_count: u64,
        highest_materialized_block: u64,
    ) -> Self {
        Self {
            key,
            encoded_len,
            digest,
            level,
            first_block,
            materialized_block_count,
            highest_materialized_block,
        }
    }

    /// Returns the exact immutable page key.
    pub const fn key(&self) -> &ObjectKey {
        &self.key
    }

    /// Returns the exact complete encoded page length.
    pub const fn encoded_len(&self) -> u64 {
        self.encoded_len
    }

    /// Returns the digest of the complete encoded page.
    pub const fn digest(&self) -> Digest {
        self.digest
    }

    /// Returns the page level, with leaves at zero.
    pub const fn level(&self) -> u8 {
        self.level
    }

    /// Returns the first block covered by the page.
    pub const fn first_block(&self) -> u64 {
        self.first_block
    }

    /// Returns the authenticated number of materialized descendant blocks.
    pub const fn materialized_block_count(&self) -> u64 {
        self.materialized_block_count
    }

    /// Returns the authenticated highest materialized descendant block.
    pub const fn highest_materialized_block(&self) -> u64 {
        self.highest_materialized_block
    }

    /// Reports whether this page covers `block_index`.
    pub fn covers(&self, block_index: u64) -> bool {
        page_contains(self.level, self.first_block, block_index)
    }
}

/// One occupied leaf slot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeafEntry {
    slot: u8,
    blob: BlobRef,
}

impl LeafEntry {
    /// Creates an occupied leaf slot.
    pub const fn new(slot: u8, blob: BlobRef) -> Self {
        Self { slot, blob }
    }

    /// Returns the slot within the leaf.
    pub const fn slot(&self) -> u8 {
        self.slot
    }

    /// Returns the selected immutable data block.
    pub const fn blob(&self) -> &BlobRef {
        &self.blob
    }
}

/// One occupied branch slot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchEntry {
    slot: u8,
    child: PageRef,
}

impl BranchEntry {
    /// Creates an occupied branch slot.
    pub const fn new(slot: u8, child: PageRef) -> Self {
        Self { slot, child }
    }

    /// Returns the slot within the branch.
    pub const fn slot(&self) -> u8 {
        self.slot
    }

    /// Returns the authenticated child reference.
    pub const fn child(&self) -> &PageRef {
        &self.child
    }
}

/// Decoded immutable mapping page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BlockMapPage {
    /// A page mapping block slots directly to payloads.
    Leaf {
        /// Owning logical file.
        file_id: FileId,
        /// Aligned first logical block.
        first_block: u64,
        /// Persisted routing profile.
        parameters: BlockSplitParameters,
        /// Sorted occupied slots.
        entries: Vec<LeafEntry>,
    },
    /// A page mapping slots to lower-level pages.
    Branch {
        /// Owning logical file.
        file_id: FileId,
        /// Page level, greater than zero.
        level: u8,
        /// Aligned first logical block.
        first_block: u64,
        /// Persisted routing profile.
        parameters: BlockSplitParameters,
        /// Sorted occupied slots.
        entries: Vec<BranchEntry>,
    },
}

impl BlockMapPage {
    /// Constructs a leaf page.
    pub fn leaf(file_id: FileId, first_block: u64, entries: Vec<LeafEntry>) -> Self {
        Self::Leaf {
            file_id,
            first_block,
            parameters: BlockSplitParameters::CURRENT,
            entries,
        }
    }

    /// Constructs a branch page.
    pub fn branch(file_id: FileId, level: u8, first_block: u64, entries: Vec<BranchEntry>) -> Self {
        Self::Branch {
            file_id,
            level,
            first_block,
            parameters: BlockSplitParameters::CURRENT,
            entries,
        }
    }

    /// Returns the owning file.
    pub const fn file_id(&self) -> FileId {
        match self {
            Self::Leaf { file_id, .. } | Self::Branch { file_id, .. } => *file_id,
        }
    }

    /// Returns the page level.
    pub const fn level(&self) -> u8 {
        match self {
            Self::Leaf { .. } => 0,
            Self::Branch { level, .. } => *level,
        }
    }

    /// Returns the aligned first block.
    pub const fn first_block(&self) -> u64 {
        match self {
            Self::Leaf { first_block, .. } | Self::Branch { first_block, .. } => *first_block,
        }
    }

    /// Returns the persisted routing profile.
    pub const fn parameters(&self) -> BlockSplitParameters {
        match self {
            Self::Leaf { parameters, .. } | Self::Branch { parameters, .. } => *parameters,
        }
    }

    /// Returns leaf entries, if this is a leaf.
    pub fn leaf_entries(&self) -> Option<&[LeafEntry]> {
        match self {
            Self::Leaf { entries, .. } => Some(entries),
            Self::Branch { .. } => None,
        }
    }

    /// Returns branch entries, if this is a branch.
    pub fn branch_entries(&self) -> Option<&[BranchEntry]> {
        match self {
            Self::Branch { entries, .. } => Some(entries),
            Self::Leaf { .. } => None,
        }
    }

    /// Derives the authenticated summary carried by a parent reference.
    pub fn summary(&self) -> Result<(u64, u64), PersistentFormatError> {
        match self {
            Self::Leaf {
                first_block,
                entries,
                ..
            } => {
                let count =
                    u64::try_from(entries.len()).map_err(|_| FormatError::ArithmeticOverflow {
                        field: "leaf count",
                    })?;
                let highest = entries.last().ok_or(FormatError::NonCanonical {
                    field: "empty mapping page",
                })?;
                let highest = first_block.checked_add(u64::from(highest.slot)).ok_or(
                    FormatError::ArithmeticOverflow {
                        field: "leaf highest block",
                    },
                )?;
                Ok((count, highest))
            }
            Self::Branch { entries, .. } => {
                let mut count = 0_u64;
                for entry in entries {
                    count = count
                        .checked_add(entry.child.materialized_block_count)
                        .ok_or(FormatError::ArithmeticOverflow {
                            field: "branch materialized count",
                        })?;
                }
                let highest = entries
                    .iter()
                    .map(|entry| entry.child.highest_materialized_block)
                    .max()
                    .ok_or(FormatError::NonCanonical {
                        field: "empty mapping page",
                    })?;
                Ok((count, highest))
            }
        }
    }
}

/// Returns the number of logical blocks covered by one page at `level`.
pub fn page_coverage(level: u8) -> Result<u64, FormatError> {
    if level > BLOCK_MAP_MAX_LEVEL {
        return Err(FormatError::NonCanonical {
            field: "page level",
        });
    }
    1_u64
        .checked_shl(7 * (u32::from(level) + 1))
        .ok_or(FormatError::ArithmeticOverflow {
            field: "page coverage",
        })
}

/// Returns the minimum root level that covers `highest_block` from block zero.
pub fn minimum_root_level(highest_block: u64) -> Result<u8, FormatError> {
    for level in 0..=BLOCK_MAP_MAX_LEVEL {
        if highest_block < page_coverage(level)? {
            return Ok(level);
        }
    }
    Err(FormatError::NonCanonical {
        field: "block index outside radix domain",
    })
}

/// Returns the slot selected by `block_index` within a page.
pub fn page_slot(level: u8, first_block: u64, block_index: u64) -> Result<u8, FormatError> {
    if !page_contains(level, first_block, block_index) {
        return Err(FormatError::NonCanonical {
            field: "block outside page range",
        });
    }
    let child_coverage = if level == 0 {
        1
    } else {
        page_coverage(level - 1)?
    };
    let slot = (block_index - first_block) / child_coverage;
    u8::try_from(slot).map_err(|_| FormatError::ArithmeticOverflow { field: "page slot" })
}

/// Returns the aligned page start containing `block_index` at `level`.
pub fn aligned_page_start(level: u8, block_index: u64) -> Result<u64, FormatError> {
    let coverage = page_coverage(level)?;
    Ok(block_index / coverage * coverage)
}

fn page_contains(level: u8, first_block: u64, block_index: u64) -> bool {
    page_coverage(level)
        .ok()
        .and_then(|coverage| first_block.checked_add(coverage))
        .is_some_and(|end| block_index >= first_block && block_index < end)
}

pub(crate) fn validate_page_ref(
    reference: &PageRef,
    limits: StorageLimits,
) -> Result<(), PersistentFormatError> {
    checked_key_length(&reference.key, limits)?;
    let encoded_limit = u64::try_from(limits.max_map_page_bytes()).unwrap_or(u64::MAX);
    if reference.encoded_len > encoded_limit {
        return Err(
            LimitError::new(LimitKind::MapPage, reference.encoded_len, encoded_limit).into(),
        );
    }
    let minimum_encoded_len = if reference.level == 0 {
        MINIMUM_LEAF_PAGE_BYTES
    } else {
        MINIMUM_BRANCH_PAGE_BYTES
    }
    .checked_add(crate::representation::MAP_PAGE_MIN_OVERHEAD)
    .ok_or(FormatError::ArithmeticOverflow {
        field: "mapping page minimum representation length",
    })?;
    if reference.encoded_len < u64::try_from(minimum_encoded_len).unwrap() {
        return Err(FormatError::InconsistentLength {
            field: "mapping page",
        }
        .into());
    }
    let coverage = page_coverage(reference.level)?;
    if !reference.first_block.is_multiple_of(coverage) {
        return Err(FormatError::NonCanonical {
            field: "page alignment",
        }
        .into());
    }
    let domain = 1_u64 << 49;
    if reference.first_block >= domain
        || reference
            .first_block
            .checked_add(coverage)
            .is_none_or(|end| end > domain)
    {
        return Err(FormatError::NonCanonical {
            field: "page range",
        }
        .into());
    }
    if reference.materialized_block_count == 0
        || reference.materialized_block_count > coverage
        || reference.materialized_block_count > limits.max_materialized_blocks()
    {
        return Err(FormatError::NonCanonical {
            field: "page materialized count",
        }
        .into());
    }
    if !page_contains(
        reference.level,
        reference.first_block,
        reference.highest_materialized_block,
    ) || reference.materialized_block_count
        > reference.highest_materialized_block - reference.first_block + 1
    {
        return Err(FormatError::NonCanonical {
            field: "page highest block",
        }
        .into());
    }
    Ok(())
}

pub(crate) fn page_ref_encoded_len(
    reference: &PageRef,
    limits: StorageLimits,
) -> Result<usize, PersistentFormatError> {
    let key = usize::try_from(checked_key_length(&reference.key, limits)?).map_err(|_| {
        FormatError::ArithmeticOverflow {
            field: "page key length",
        }
    })?;
    4_usize
        .checked_add(key)
        .and_then(|length| length.checked_add(8 + 32 + 1 + 8 + 8 + 8))
        .ok_or_else(|| {
            FormatError::ArithmeticOverflow {
                field: "page reference length",
            }
            .into()
        })
}

pub(crate) fn write_page_ref(
    writer: &mut Writer,
    reference: &PageRef,
    limits: StorageLimits,
) -> Result<(), PersistentFormatError> {
    validate_page_ref(reference, limits)?;
    write_key(writer, &reference.key, limits)?;
    writer.write_u64(reference.encoded_len);
    writer.write_bytes(reference.digest.as_bytes());
    writer.write_u8(reference.level);
    writer.write_u64(reference.first_block);
    writer.write_u64(reference.materialized_block_count);
    writer.write_u64(reference.highest_materialized_block);
    Ok(())
}

pub(crate) fn read_page_ref(
    reader: &mut Reader<'_>,
    limits: StorageLimits,
) -> Result<PageRef, PersistentFormatError> {
    let reference = PageRef {
        key: read_key(reader, limits)?,
        encoded_len: reader.read_u64()?,
        digest: Digest::new(reader.read_array()?),
        level: reader.read_u8()?,
        first_block: reader.read_u64()?,
        materialized_block_count: reader.read_u64()?,
        highest_materialized_block: reader.read_u64()?,
    };
    validate_page_ref(&reference, limits)?;
    Ok(reference)
}

/// Encodes one canonical immutable mapping page.
pub fn encode_block_map_page(
    page: &BlockMapPage,
    limits: StorageLimits,
) -> Result<Vec<u8>, PersistentFormatError> {
    validate_page(page, limits)?;
    let payload_len = page_payload_len(page, limits)?;
    checked_envelope_len(payload_len, limits.max_map_page_bytes(), LimitKind::MapPage)?;
    let mut writer = Writer::with_capacity(payload_len);
    writer.write_bytes(page.file_id().as_bytes());
    writer.write_u8(page.level());
    writer.write_u64(page.first_block());
    write_profile(&mut writer, page.parameters());
    let count = match page {
        BlockMapPage::Leaf { entries, .. } => entries.len(),
        BlockMapPage::Branch { entries, .. } => entries.len(),
    };
    writer.write_u16(
        u16::try_from(count).map_err(|_| FormatError::ArithmeticOverflow {
            field: "page entry count",
        })?,
    );
    match page {
        BlockMapPage::Leaf { entries, .. } => {
            for entry in entries {
                writer.write_u8(entry.slot);
                write_blob(&mut writer, &entry.blob, limits)?;
            }
        }
        BlockMapPage::Branch { entries, .. } => {
            for entry in entries {
                writer.write_u8(entry.slot);
                write_page_ref(&mut writer, &entry.child, limits)?;
            }
        }
    }
    let payload = writer.into_bytes();
    let kind = if page.level() == 0 {
        ObjectKind::LeafMap
    } else {
        ObjectKind::BranchMap
    };
    encode_envelope(kind, &payload, limits.max_map_page_bytes()).map_err(|error| {
        LimitError::new(
            LimitKind::MapPage,
            error.actual,
            u64::try_from(limits.max_map_page_bytes()).unwrap_or(u64::MAX),
        )
        .into()
    })
}

/// Decodes one checked mapping page, choosing its kind from `expected_level`.
pub fn decode_block_map_page(
    bytes: &[u8],
    expected_level: u8,
    limits: StorageLimits,
) -> Result<BlockMapPage, PersistentFormatError> {
    if bytes.len() > limits.max_map_page_bytes() {
        return Err(LimitError::new(
            LimitKind::MapPage,
            u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            u64::try_from(limits.max_map_page_bytes()).unwrap_or(u64::MAX),
        )
        .into());
    }
    let kind = if expected_level == 0 {
        ObjectKind::LeafMap
    } else {
        ObjectKind::BranchMap
    };
    let envelope = decode_envelope(kind, bytes, limits.max_map_page_bytes())?;
    let mut reader = Reader::new(envelope.payload());
    let file_id = FileId::new(reader.read_array()?);
    let level = reader.read_u8()?;
    let first_block = reader.read_u64()?;
    let parameters = read_profile(&mut reader)?;
    let count = reader.read_u16()?;
    if count == 0 || count > BLOCK_MAP_FANOUT {
        return Err(FormatError::NonCanonical {
            field: "mapping page entry count",
        }
        .into());
    }
    let count = usize::from(count);
    let minimum_entry = if expected_level == 0 {
        1 + MINIMUM_BLOB_REF_BYTES
    } else {
        1 + MINIMUM_PAGE_REF_BYTES
    };
    if count > reader.remaining() / minimum_entry {
        return Err(FormatError::InconsistentLength {
            field: "mapping page entries",
        }
        .into());
    }
    let page = if expected_level == 0 {
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            entries.push(LeafEntry::new(
                reader.read_u8()?,
                read_blob(&mut reader, limits)?,
            ));
        }
        BlockMapPage::Leaf {
            file_id,
            first_block,
            parameters,
            entries,
        }
    } else {
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            entries.push(BranchEntry::new(
                reader.read_u8()?,
                read_page_ref(&mut reader, limits)?,
            ));
        }
        BlockMapPage::Branch {
            file_id,
            level,
            first_block,
            parameters,
            entries,
        }
    };
    reader.finish()?;
    validate_page(&page, limits)?;
    if page.level() != expected_level {
        return Err(FormatError::NonCanonical {
            field: "mapping page level",
        }
        .into());
    }
    Ok(page)
}

/// Validates a decoded page against the exact parent-selected reference and file.
pub fn validate_page_context(
    page: &BlockMapPage,
    reference: &PageRef,
    expected_file: FileId,
    limits: StorageLimits,
) -> Result<(), PersistentFormatError> {
    validate_page_ref(reference, limits)?;
    if page.file_id() != expected_file {
        return Err(CorruptionError::IdentityMismatch {
            field: "mapping page file",
        }
        .into());
    }
    if page.level() != reference.level || page.first_block() != reference.first_block {
        return Err(CorruptionError::IdentityMismatch {
            field: "mapping page location",
        }
        .into());
    }
    let (count, highest) = page.summary()?;
    if count != reference.materialized_block_count
        || highest != reference.highest_materialized_block
    {
        return Err(CorruptionError::IdentityMismatch {
            field: "mapping page summary",
        }
        .into());
    }
    Ok(())
}

fn page_payload_len(
    page: &BlockMapPage,
    limits: StorageLimits,
) -> Result<usize, PersistentFormatError> {
    let mut length = PAGE_HEADER_BYTES;
    match page {
        BlockMapPage::Leaf { entries, .. } => {
            for entry in entries {
                length = length
                    .checked_add(1)
                    .and_then(|v| v.checked_add(blob_encoded_len(&entry.blob, limits).ok()?))
                    .ok_or(FormatError::ArithmeticOverflow {
                        field: "leaf page length",
                    })?;
            }
        }
        BlockMapPage::Branch { entries, .. } => {
            for entry in entries {
                length = length
                    .checked_add(1)
                    .and_then(|v| v.checked_add(page_ref_encoded_len(&entry.child, limits).ok()?))
                    .ok_or(FormatError::ArithmeticOverflow {
                        field: "branch page length",
                    })?;
            }
        }
    }
    Ok(length)
}

fn validate_page(page: &BlockMapPage, limits: StorageLimits) -> Result<(), PersistentFormatError> {
    if page.parameters() != BlockSplitParameters::CURRENT {
        return Err(FormatError::NonCanonical {
            field: "mapping page profile",
        }
        .into());
    }
    let level = page.level();
    let coverage = page_coverage(level)?;
    if !page.first_block().is_multiple_of(coverage) {
        return Err(FormatError::NonCanonical {
            field: "mapping page alignment",
        }
        .into());
    }
    let domain = 1_u64 << 49;
    if page.first_block() >= domain
        || page
            .first_block()
            .checked_add(coverage)
            .is_none_or(|end| end > domain)
    {
        return Err(FormatError::NonCanonical {
            field: "mapping page range",
        }
        .into());
    }
    match page {
        BlockMapPage::Leaf {
            entries,
            first_block,
            ..
        } => {
            validate_slots(entries.iter().map(|entry| entry.slot))?;
            let zero_digest = Digest::blake3(&[0; crate::BLOCK_SIZE as usize]);
            for entry in entries {
                validate_blob(&entry.blob, limits)?;
                if entry.blob.plaintext_len() != u64::from(crate::BLOCK_SIZE) {
                    return Err(FormatError::InconsistentLength {
                        field: "block plaintext",
                    }
                    .into());
                }
                if entry.blob.digest() == zero_digest {
                    return Err(FormatError::NonCanonical {
                        field: "materialized zero block",
                    }
                    .into());
                }
                let _ = first_block.checked_add(u64::from(entry.slot)).ok_or(
                    FormatError::ArithmeticOverflow {
                        field: "leaf block index",
                    },
                )?;
            }
        }
        BlockMapPage::Branch {
            level,
            first_block,
            entries,
            ..
        } => {
            if *level == 0 {
                return Err(FormatError::NonCanonical {
                    field: "branch level",
                }
                .into());
            }
            validate_slots(entries.iter().map(|entry| entry.slot))?;
            let child_coverage = page_coverage(level - 1)?;
            for entry in entries {
                validate_page_ref(&entry.child, limits)?;
                let expected_first = first_block
                    .checked_add(u64::from(entry.slot) * child_coverage)
                    .ok_or(FormatError::ArithmeticOverflow {
                        field: "child page range",
                    })?;
                if entry.child.level != level - 1 || entry.child.first_block != expected_first {
                    return Err(FormatError::NonCanonical {
                        field: "child page location",
                    }
                    .into());
                }
            }
        }
    }
    Ok(())
}

fn validate_slots(slots: impl Iterator<Item = u8>) -> Result<(), PersistentFormatError> {
    let mut previous = None;
    let mut any = false;
    for slot in slots {
        any = true;
        if u16::from(slot) >= BLOCK_MAP_FANOUT || previous.is_some_and(|prior| slot <= prior) {
            return Err(FormatError::NonCanonical {
                field: "mapping page slot ordering",
            }
            .into());
        }
        previous = Some(slot);
    }
    if !any {
        return Err(FormatError::NonCanonical {
            field: "empty mapping page",
        }
        .into());
    }
    Ok(())
}

pub(crate) fn write_profile(writer: &mut Writer, parameters: BlockSplitParameters) {
    writer.write_u32(parameters.block_size());
    writer.write_u16(parameters.fanout());
    writer.write_u8(parameters.index_bits());
    writer.write_u8(parameters.max_level());
}

pub(crate) fn read_profile(
    reader: &mut Reader<'_>,
) -> Result<BlockSplitParameters, PersistentFormatError> {
    let block_size = reader.read_u32()?;
    let fanout = reader.read_u16()?;
    let index_bits = reader.read_u8()?;
    let max_level = reader.read_u8()?;
    BlockSplitParameters::from_persisted(block_size, fanout, index_bits, max_level).map_err(|_| {
        FormatError::UnknownTag {
            field: "block map profile",
            tag: u64::from(block_size),
        }
        .into()
    })
}
