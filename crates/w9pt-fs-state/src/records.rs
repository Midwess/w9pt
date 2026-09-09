//! Checked authoritative filesystem records.

use core::fmt;
use std::collections::{BTreeMap, BTreeSet};

use crate::{
    ClientIncarnationId, ContentMetadataRecord, DataGeneration, DirectoryCookie,
    DirectoryGeneration, EntryName, FencingToken, FilesystemId, GroupId, InodeGeneration, InodeId,
    LeaseDeadline, LeaseId, LockGeneration, LockId, MutationResult, MutationRetention, OpenId,
    PrincipalId, QidPath, RecordRevision, RequestFingerprint, StateLimitError, StateLimitKind,
    StateLimits, StateRevision, SymlinkTarget, UnixTimestamp, WriterIncarnationId, WriterScopeId,
    XattrName, XattrStagingId, XattrValue,
};

/// Stable semantic family of one authoritative record.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RecordFamily {
    /// Filesystem header/root record.
    Filesystem,
    /// Inode metadata.
    Inode,
    /// Opaque per-file storage policy and wrapped-key metadata.
    ContentMetadata,
    /// Directory component mapping.
    DirectoryEntry,
    /// Portable open state.
    Open,
    /// Durable open-lifetime pin.
    OpenPin,
    /// Unlinked but retained inode.
    Orphan,
    /// Cross-session byte-range lock.
    Lock,
    /// Published extended attribute.
    Xattr,
    /// Extended-attribute staging state.
    XattrStaging,
    /// Idempotent mutation ledger result.
    Mutation,
    /// Current writer lease.
    WriterLease,
}

/// Semantic key for every public authoritative record family.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RecordKey {
    /// Filesystem authority header.
    Filesystem(FilesystemId),
    /// Inode by stable identity.
    Inode(FilesystemId, InodeId),
    /// Content metadata by stable storage file identity.
    ContentMetadata(FilesystemId, w9pt_fs_storage::FileId),
    /// Directory entry by byte-exact component.
    DirectoryEntry(FilesystemId, InodeId, EntryName),
    /// Open by portable identity.
    Open(FilesystemId, OpenId),
    /// Open pin by inode and open identity.
    OpenPin(FilesystemId, InodeId, OpenId),
    /// Orphan by inode identity.
    Orphan(FilesystemId, InodeId),
    /// Lock by inode and stable lock identity.
    Lock(FilesystemId, InodeId, LockId),
    /// Xattr by inode and byte-exact name.
    Xattr(FilesystemId, InodeId, XattrName),
    /// Xattr staging by stable staging identity.
    XattrStaging(FilesystemId, XattrStagingId),
    /// Mutation ledger by globally stable mutation identity.
    Mutation(FilesystemId, w9pt_fs_storage::MutationId),
    /// Writer lease by protected scope.
    WriterLease(FilesystemId, WriterScopeId),
}

impl RecordKey {
    /// Returns the filesystem owning this key.
    pub const fn filesystem_id(&self) -> FilesystemId {
        match self {
            Self::Filesystem(filesystem_id)
            | Self::Inode(filesystem_id, _)
            | Self::ContentMetadata(filesystem_id, _)
            | Self::DirectoryEntry(filesystem_id, _, _)
            | Self::Open(filesystem_id, _)
            | Self::OpenPin(filesystem_id, _, _)
            | Self::Orphan(filesystem_id, _)
            | Self::Lock(filesystem_id, _, _)
            | Self::Xattr(filesystem_id, _, _)
            | Self::XattrStaging(filesystem_id, _)
            | Self::Mutation(filesystem_id, _)
            | Self::WriterLease(filesystem_id, _) => *filesystem_id,
        }
    }

    /// Returns the semantic record family selected by this key.
    pub const fn family(&self) -> RecordFamily {
        match self {
            Self::Filesystem(_) => RecordFamily::Filesystem,
            Self::Inode(_, _) => RecordFamily::Inode,
            Self::ContentMetadata(_, _) => RecordFamily::ContentMetadata,
            Self::DirectoryEntry(_, _, _) => RecordFamily::DirectoryEntry,
            Self::Open(_, _) => RecordFamily::Open,
            Self::OpenPin(_, _, _) => RecordFamily::OpenPin,
            Self::Orphan(_, _) => RecordFamily::Orphan,
            Self::Lock(_, _, _) => RecordFamily::Lock,
            Self::Xattr(_, _, _) => RecordFamily::Xattr,
            Self::XattrStaging(_, _) => RecordFamily::XattrStaging,
            Self::Mutation(_, _) => RecordFamily::Mutation,
            Self::WriterLease(_, _) => RecordFamily::WriterLease,
        }
    }

    pub(crate) fn retained_bytes(&self) -> Option<usize> {
        let variable = match self {
            Self::DirectoryEntry(_, _, name) => name.as_bytes().len(),
            Self::Xattr(_, _, name) => name.as_bytes().len(),
            _ => 0,
        };
        64usize.checked_add(variable)
    }

    /// Revalidates variable key fields against one adapter's configured limits.
    pub fn validate_against_limits(&self, limits: StateLimits) -> Result<(), StateLimitError> {
        match self {
            Self::DirectoryEntry(_, _, name) => limits.require_bytes(
                StateLimitKind::EntryName,
                name.as_bytes().len(),
                limits.max_entry_name_bytes(),
            ),
            Self::Xattr(_, _, name) => limits.require_bytes(
                StateLimitKind::XattrName,
                name.as_bytes().len(),
                limits.max_xattr_name_bytes(),
            ),
            _ => Ok(()),
        }
    }
}

/// Tagged authoritative record value with no adapter-specific representation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StateRecord {
    /// Filesystem header/root record.
    Filesystem(FilesystemRecord),
    /// Inode metadata.
    Inode(InodeRecord),
    /// Opaque per-file context metadata.
    ContentMetadata(ContentMetadataRecord),
    /// Directory component mapping.
    DirectoryEntry(DirectoryEntryRecord),
    /// Portable open state.
    Open(OpenRecord),
    /// Durable open-lifetime pin.
    OpenPin(OpenPinRecord),
    /// Unlinked but retained inode.
    Orphan(OrphanRecord),
    /// Cross-session byte-range lock.
    Lock(LockRecord),
    /// Published extended attribute.
    Xattr(XattrRecord),
    /// Extended-attribute staging state.
    XattrStaging(XattrStagingRecord),
    /// Idempotent mutation ledger result.
    Mutation(MutationRecord),
    /// Current writer lease.
    WriterLease(WriterLeaseRecord),
}

impl StateRecord {
    /// Returns the semantic family encoded by this value.
    pub const fn family(&self) -> RecordFamily {
        match self {
            Self::Filesystem(_) => RecordFamily::Filesystem,
            Self::Inode(_) => RecordFamily::Inode,
            Self::ContentMetadata(_) => RecordFamily::ContentMetadata,
            Self::DirectoryEntry(_) => RecordFamily::DirectoryEntry,
            Self::Open(_) => RecordFamily::Open,
            Self::OpenPin(_) => RecordFamily::OpenPin,
            Self::Orphan(_) => RecordFamily::Orphan,
            Self::Lock(_) => RecordFamily::Lock,
            Self::Xattr(_) => RecordFamily::Xattr,
            Self::XattrStaging(_) => RecordFamily::XattrStaging,
            Self::Mutation(_) => RecordFamily::Mutation,
            Self::WriterLease(_) => RecordFamily::WriterLease,
        }
    }

    /// Returns the authoritative record revision.
    pub const fn revision(&self) -> RecordRevision {
        match self {
            Self::Filesystem(record) => record.record_revision(),
            Self::Inode(record) => record.revision(),
            Self::ContentMetadata(record) => record.revision(),
            Self::DirectoryEntry(record) => record.revision(),
            Self::Open(record) => record.revision(),
            Self::OpenPin(record) => record.revision(),
            Self::Orphan(record) => record.revision(),
            Self::Lock(record) => record.revision(),
            Self::Xattr(record) => record.revision(),
            Self::XattrStaging(record) => record.revision(),
            Self::Mutation(record) => record.revision(),
            Self::WriterLease(record) => record.revision(),
        }
    }

    /// Validates exact variant and semantic identities against a key.
    pub fn validate_key(&self, key: &RecordKey) -> Result<(), RecordValidationError> {
        if self.family() != key.family() {
            return Err(RecordValidationError::KeyVariantMismatch {
                key: key.family(),
                value: self.family(),
            });
        }
        let matches = match (key, self) {
            (RecordKey::Filesystem(filesystem_id), Self::Filesystem(record)) => {
                *filesystem_id == record.filesystem_id()
            }
            (RecordKey::Inode(_, inode_id), Self::Inode(record)) => *inode_id == record.inode_id(),
            (RecordKey::ContentMetadata(_, file_id), Self::ContentMetadata(record)) => {
                *file_id == record.content_file_id()
            }
            (RecordKey::DirectoryEntry(_, parent, name), Self::DirectoryEntry(record)) => {
                *parent == record.parent_inode_id() && name == record.name()
            }
            (RecordKey::Open(_, open_id), Self::Open(record)) => *open_id == record.open_id(),
            (RecordKey::OpenPin(_, inode_id, open_id), Self::OpenPin(record)) => {
                *inode_id == record.inode_id() && *open_id == record.open_id()
            }
            (RecordKey::Orphan(_, inode_id), Self::Orphan(record)) => {
                *inode_id == record.inode_id()
            }
            (RecordKey::Lock(_, inode_id, lock_id), Self::Lock(record)) => {
                *inode_id == record.inode_id() && *lock_id == record.lock_id()
            }
            (RecordKey::Xattr(_, inode_id, name), Self::Xattr(record)) => {
                *inode_id == record.inode_id() && name == record.name()
            }
            (RecordKey::XattrStaging(_, staging_id), Self::XattrStaging(record)) => {
                *staging_id == record.staging_id()
            }
            (RecordKey::Mutation(filesystem_id, mutation_id), Self::Mutation(record)) => {
                *filesystem_id == record.filesystem_id() && *mutation_id == record.mutation_id()
            }
            (RecordKey::WriterLease(filesystem_id, scope), Self::WriterLease(record)) => {
                *filesystem_id == record.filesystem_id() && *scope == record.scope()
            }
            _ => false,
        };
        if matches {
            Ok(())
        } else {
            Err(RecordValidationError::KeyIdentityMismatch {
                family: key.family(),
            })
        }
    }

    pub(crate) fn with_revision(mut self, revision: RecordRevision) -> Self {
        match &mut self {
            Self::Filesystem(record) => {
                record.record_revision = revision;
                record.revision = StateRevision::new(revision.get())
                    .expect("a record revision is always nonzero");
            }
            Self::Inode(record) => record.revision = revision,
            Self::ContentMetadata(record) => *record = record.clone().with_revision(revision),
            Self::DirectoryEntry(record) => record.revision = revision,
            Self::Open(record) => record.revision = revision,
            Self::OpenPin(record) => record.revision = revision,
            Self::Orphan(record) => record.revision = revision,
            Self::Lock(record) => record.revision = revision,
            Self::Xattr(record) => record.revision = revision,
            Self::XattrStaging(record) => record.revision = revision,
            Self::Mutation(record) => record.revision = revision,
            Self::WriterLease(record) => record.revision = revision,
        }
        self
    }

    pub(crate) fn retained_bytes(&self) -> Option<usize> {
        let (fixed, variable) = match self {
            Self::Filesystem(_) => (128usize, 0usize),
            Self::Inode(record) => {
                let mut variable = record.owner().as_bytes().len();
                variable = variable.checked_add(record.group().as_bytes().len())?;
                match record.data() {
                    InodeData::RegularFile {
                        content: Some(content),
                        ..
                    } => {
                        variable = variable.checked_add(content.manifest_key().as_str().len())?;
                    }
                    InodeData::Symlink { target } => {
                        variable = variable.checked_add(target.as_bytes().len())?;
                    }
                    _ => {}
                }
                (256, variable)
            }
            Self::ContentMetadata(record) => return record.retained_bytes(),
            Self::DirectoryEntry(record) => (96, record.name().as_bytes().len()),
            Self::Open(_) | Self::OpenPin(_) | Self::Orphan(_) | Self::Lock(_) => (128, 0),
            Self::Xattr(record) => (
                96,
                record
                    .name()
                    .as_bytes()
                    .len()
                    .checked_add(record.value().as_bytes().len())?,
            ),
            Self::XattrStaging(record) => (
                112,
                record
                    .name()
                    .as_bytes()
                    .len()
                    .checked_add(record.bytes().as_bytes().len())?,
            ),
            Self::Mutation(record) => (256, record.result().retained_bytes()),
            Self::WriterLease(_) => (128, 0),
        };
        fixed.checked_add(variable)
    }

    /// Revalidates every nested variable field against one adapter's limits.
    pub fn validate_against_limits(&self, limits: StateLimits) -> Result<(), StateLimitError> {
        match self {
            Self::Inode(record) => {
                limits.require_bytes(
                    StateLimitKind::Principal,
                    record.owner().as_bytes().len(),
                    limits.max_principal_bytes(),
                )?;
                limits.require_bytes(
                    StateLimitKind::Group,
                    record.group().as_bytes().len(),
                    limits.max_group_bytes(),
                )?;
                if let InodeData::Symlink { target } = record.data() {
                    limits.require_bytes(
                        StateLimitKind::Symlink,
                        target.as_bytes().len(),
                        limits.max_symlink_bytes(),
                    )?;
                }
                Ok(())
            }
            Self::ContentMetadata(record) => {
                limits.require_bytes(
                    StateLimitKind::ContentPolicy,
                    record.policy_bytes().len(),
                    limits.max_content_policy_bytes(),
                )?;
                limits.require_bytes(
                    StateLimitKind::WrappedContentKey,
                    record.wrapped_key_bytes().map_or(0, <[u8]>::len),
                    limits.max_wrapped_content_key_bytes(),
                )?;
                limits.require_bytes(
                    StateLimitKind::ContentMetadata,
                    record.retained_bytes().unwrap_or(usize::MAX),
                    limits.max_content_metadata_bytes(),
                )
            }
            Self::DirectoryEntry(record) => limits.require_bytes(
                StateLimitKind::EntryName,
                record.name().as_bytes().len(),
                limits.max_entry_name_bytes(),
            ),
            Self::Xattr(record) => {
                limits.require_bytes(
                    StateLimitKind::XattrName,
                    record.name().as_bytes().len(),
                    limits.max_xattr_name_bytes(),
                )?;
                limits.require_bytes(
                    StateLimitKind::XattrValue,
                    record.value().as_bytes().len(),
                    limits.max_xattr_value_bytes(),
                )
            }
            Self::XattrStaging(record) => {
                limits.require_bytes(
                    StateLimitKind::XattrName,
                    record.name().as_bytes().len(),
                    limits.max_xattr_name_bytes(),
                )?;
                limits.require_bytes(
                    StateLimitKind::XattrValue,
                    usize::try_from(record.expected_size()).unwrap_or(usize::MAX),
                    limits.max_xattr_value_bytes(),
                )?;
                limits.require_bytes(
                    StateLimitKind::XattrValue,
                    record.bytes().as_bytes().len(),
                    limits.max_xattr_value_bytes(),
                )
            }
            Self::Mutation(record) => limits.require_bytes(
                StateLimitKind::MutationResult,
                record.result().retained_bytes(),
                limits.max_mutation_result_bytes(),
            ),
            _ => Ok(()),
        }
    }
}

/// Authoritative filesystem root and allocation state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilesystemRecord {
    filesystem_id: FilesystemId,
    revision: StateRevision,
    record_revision: RecordRevision,
    root_inode_id: InodeId,
    next_qid_path: QidPath,
    next_directory_cookie: DirectoryCookie,
    policy_generation: u64,
}

impl FilesystemRecord {
    /// Creates a checked filesystem authority record.
    pub fn new(
        filesystem_id: FilesystemId,
        revision: StateRevision,
        record_revision: RecordRevision,
        root_inode_id: InodeId,
        next_qid_path: QidPath,
        next_directory_cookie: DirectoryCookie,
        policy_generation: u64,
    ) -> Result<Self, RecordValidationError> {
        if next_directory_cookie == DirectoryCookie::START {
            return Err(RecordValidationError::ZeroDirectoryCookie);
        }
        if policy_generation == 0 {
            return Err(RecordValidationError::ZeroGeneration {
                field: "filesystem policy",
            });
        }
        Ok(Self {
            filesystem_id,
            revision,
            record_revision,
            root_inode_id,
            next_qid_path,
            next_directory_cookie,
            policy_generation,
        })
    }

    /// Returns the stable filesystem identity.
    pub const fn filesystem_id(&self) -> FilesystemId {
        self.filesystem_id
    }

    /// Returns the current authoritative state revision.
    pub const fn revision(&self) -> StateRevision {
        self.revision
    }

    /// Returns the record revision of this filesystem header.
    pub const fn record_revision(&self) -> RecordRevision {
        self.record_revision
    }

    /// Returns the stable root inode.
    pub const fn root_inode_id(&self) -> InodeId {
        self.root_inode_id
    }

    /// Returns the next stable QID path that may be allocated transactionally.
    pub const fn next_qid_path(&self) -> QidPath {
        self.next_qid_path
    }

    /// Returns the next cookie that may be allocated transactionally.
    pub const fn next_directory_cookie(&self) -> DirectoryCookie {
        self.next_directory_cookie
    }

    /// Returns the filesystem policy/configuration generation.
    pub const fn policy_generation(&self) -> u64 {
        self.policy_generation
    }

    pub(crate) fn advance_directory_cookie(
        &mut self,
        count: core::num::NonZeroU64,
    ) -> Result<(), crate::CounterOverflow> {
        self.next_directory_cookie = self.next_directory_cookie.checked_advance(count.get())?;
        Ok(())
    }

    pub(crate) fn advance_qid_path(
        &mut self,
        count: core::num::NonZeroU64,
    ) -> Result<(), crate::CounterOverflow> {
        self.next_qid_path = self.next_qid_path.checked_advance(count.get())?;
        Ok(())
    }

    pub(crate) fn bump_policy_generation(&mut self) -> Result<(), crate::CounterOverflow> {
        self.policy_generation =
            self.policy_generation
                .checked_add(1)
                .ok_or(crate::CounterOverflow {
                    field: "FilesystemPolicyGeneration",
                })?;
        Ok(())
    }
}

/// Authoritative mapping from one directory component to a stable child inode.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectoryEntryRecord {
    parent_inode_id: InodeId,
    name: EntryName,
    cookie: DirectoryCookie,
    child_inode_id: InodeId,
    revision: RecordRevision,
}

/// Access mode retained by one portable open instance.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum OpenAccess {
    /// Data may only be read.
    ReadOnly,
    /// Data may only be written.
    WriteOnly,
    /// Data may be read and written.
    ReadWrite,
    /// Directory entries may be enumerated.
    DirectoryRead,
}

/// Portable authoritative open state resolvable by any processing node.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenRecord {
    open_id: OpenId,
    inode_id: InodeId,
    client_incarnation: ClientIncarnationId,
    access: OpenAccess,
    append: bool,
    retained_inode_generation: InodeGeneration,
    revision: RecordRevision,
}

impl OpenRecord {
    /// Creates one portable open record.
    pub const fn new(
        open_id: OpenId,
        inode_id: InodeId,
        client_incarnation: ClientIncarnationId,
        access: OpenAccess,
        append: bool,
        retained_inode_generation: InodeGeneration,
        revision: RecordRevision,
    ) -> Self {
        Self {
            open_id,
            inode_id,
            client_incarnation,
            access,
            append,
            retained_inode_generation,
            revision,
        }
    }

    /// Returns the stable open identity.
    pub const fn open_id(&self) -> OpenId {
        self.open_id
    }

    /// Returns the opened inode.
    pub const fn inode_id(&self) -> InodeId {
        self.inode_id
    }

    /// Returns the owning client/session lineage.
    pub const fn client_incarnation(&self) -> ClientIncarnationId {
        self.client_incarnation
    }

    /// Returns permitted open access.
    pub const fn access(&self) -> OpenAccess {
        self.access
    }

    /// Reports whether append offset selection is required by the future engine.
    pub const fn append(&self) -> bool {
        self.append
    }

    /// Returns the inode generation retained when the open was established.
    pub const fn retained_inode_generation(&self) -> InodeGeneration {
        self.retained_inode_generation
    }

    /// Returns the record revision.
    pub const fn revision(&self) -> RecordRevision {
        self.revision
    }
}

/// Durable relationship pinning one inode for one portable open.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenPinRecord {
    inode_id: InodeId,
    open_id: OpenId,
    revision: RecordRevision,
}

impl OpenPinRecord {
    /// Creates a durable open pin.
    pub const fn new(inode_id: InodeId, open_id: OpenId, revision: RecordRevision) -> Self {
        Self {
            inode_id,
            open_id,
            revision,
        }
    }

    /// Returns the pinned inode.
    pub const fn inode_id(&self) -> InodeId {
        self.inode_id
    }

    /// Returns the pinning open identity.
    pub const fn open_id(&self) -> OpenId {
        self.open_id
    }

    /// Returns the record revision.
    pub const fn revision(&self) -> RecordRevision {
        self.revision
    }
}

/// Unlinked inode retained by one or more durable open pins.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrphanRecord {
    inode_id: InodeId,
    open_pin_count: u64,
    orphaned_revision: StateRevision,
    revision: RecordRevision,
}

impl OrphanRecord {
    /// Creates an orphan record with at least one retaining pin.
    pub fn new(
        inode_id: InodeId,
        open_pin_count: u64,
        orphaned_revision: StateRevision,
        revision: RecordRevision,
    ) -> Result<Self, RecordValidationError> {
        if open_pin_count == 0 {
            return Err(RecordValidationError::ZeroOpenPins);
        }
        Ok(Self {
            inode_id,
            open_pin_count,
            orphaned_revision,
            revision,
        })
    }

    /// Returns the unlinked inode.
    pub const fn inode_id(&self) -> InodeId {
        self.inode_id
    }

    /// Returns the authoritative number of retaining open pins.
    pub const fn open_pin_count(&self) -> u64 {
        self.open_pin_count
    }

    /// Returns the state revision that created the orphan.
    pub const fn orphaned_revision(&self) -> StateRevision {
        self.orphaned_revision
    }

    /// Returns the record revision.
    pub const fn revision(&self) -> RecordRevision {
        self.revision
    }

    pub(crate) fn adjust_open_pin_count(
        &mut self,
        adjustment: crate::CounterAdjustment,
    ) -> Result<(), crate::CounterOverflow> {
        self.open_pin_count = adjustment
            .apply(self.open_pin_count)
            .filter(|count| *count != 0)
            .ok_or(crate::CounterOverflow {
                field: "OrphanOpenPinCount",
            })?;
        Ok(())
    }
}

/// End of a nonempty byte-range lock interval.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LockRangeEnd {
    /// Exclusive finite endpoint.
    Exclusive(u64),
    /// All bytes from the start through logical EOF, including future growth.
    ThroughEof,
}

/// Checked nonempty byte range used by an authoritative lock.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LockRange {
    start: u64,
    end: LockRangeEnd,
}

impl LockRange {
    /// Creates a finite half-open range `[start, end)`.
    pub fn finite(start: u64, end: u64) -> Result<Self, RecordValidationError> {
        if end <= start {
            return Err(RecordValidationError::InvalidLockRange { start, end });
        }
        Ok(Self {
            start,
            end: LockRangeEnd::Exclusive(end),
        })
    }

    /// Creates a range extending from `start` through EOF.
    pub const fn through_eof(start: u64) -> Self {
        Self {
            start,
            end: LockRangeEnd::ThroughEof,
        }
    }

    /// Converts 9P-style `(start, length)`, where zero length means through EOF.
    pub fn from_start_and_length(start: u64, length: u64) -> Result<Self, RecordValidationError> {
        if length == 0 {
            return Ok(Self::through_eof(start));
        }
        let end = start
            .checked_add(length)
            .ok_or(RecordValidationError::LockRangeOverflow { start, length })?;
        Self::finite(start, end)
    }

    /// Returns the first covered byte offset.
    pub const fn start(self) -> u64 {
        self.start
    }

    /// Returns the finite exclusive endpoint or the explicit EOF marker.
    pub const fn end(self) -> LockRangeEnd {
        self.end
    }

    /// Reports whether two ranges cover at least one common byte.
    pub const fn overlaps(self, other: Self) -> bool {
        let self_before_other_end = match other.end {
            LockRangeEnd::Exclusive(end) => self.start < end,
            LockRangeEnd::ThroughEof => true,
        };
        let other_before_self_end = match self.end {
            LockRangeEnd::Exclusive(end) => other.start < end,
            LockRangeEnd::ThroughEof => true,
        };
        self_before_other_end && other_before_self_end
    }
}

/// Compatibility mode of one byte-range lock.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LockKind {
    /// Compatible with other shared locks.
    Shared,
    /// Conflicts with every overlapping lock held by another owner.
    Exclusive,
}

/// Portable lock owner identity.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LockOwner {
    client_incarnation: ClientIncarnationId,
    open_id: OpenId,
}

impl LockOwner {
    /// Creates a cluster-resolvable lock owner.
    pub const fn new(client_incarnation: ClientIncarnationId, open_id: OpenId) -> Self {
        Self {
            client_incarnation,
            open_id,
        }
    }

    /// Returns the owning client/session lineage.
    pub const fn client_incarnation(self) -> ClientIncarnationId {
        self.client_incarnation
    }

    /// Returns the owning portable open.
    pub const fn open_id(self) -> OpenId {
        self.open_id
    }
}

/// Authoritative cross-session byte-range lock.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockRecord {
    lock_id: LockId,
    inode_id: InodeId,
    range: LockRange,
    kind: LockKind,
    owner: LockOwner,
    generation: LockGeneration,
    revision: RecordRevision,
}

impl LockRecord {
    /// Creates a portable byte-range lock record.
    pub const fn new(
        lock_id: LockId,
        inode_id: InodeId,
        range: LockRange,
        kind: LockKind,
        owner: LockOwner,
        generation: LockGeneration,
        revision: RecordRevision,
    ) -> Self {
        Self {
            lock_id,
            inode_id,
            range,
            kind,
            owner,
            generation,
            revision,
        }
    }

    /// Returns the stable lock identity.
    pub const fn lock_id(&self) -> LockId {
        self.lock_id
    }

    /// Returns the locked inode.
    pub const fn inode_id(&self) -> InodeId {
        self.inode_id
    }

    /// Returns the covered byte range.
    pub const fn range(&self) -> LockRange {
        self.range
    }

    /// Returns the compatibility mode.
    pub const fn kind(&self) -> LockKind {
        self.kind
    }

    /// Returns the portable owner identity.
    pub const fn owner(&self) -> LockOwner {
        self.owner
    }

    /// Returns the lock generation.
    pub const fn generation(&self) -> LockGeneration {
        self.generation
    }

    /// Returns the record revision.
    pub const fn revision(&self) -> RecordRevision {
        self.revision
    }

    /// Reports a deterministic incompatibility with another lock record.
    pub fn conflicts_with(&self, other: &Self) -> bool {
        self.inode_id == other.inode_id
            && self.owner != other.owner
            && self.range.overlaps(other.range)
            && (matches!(self.kind, LockKind::Exclusive)
                || matches!(other.kind, LockKind::Exclusive))
    }

    pub(crate) fn bump_generation(&mut self) -> Result<(), crate::CounterOverflow> {
        self.generation = self.generation.checked_next()?;
        Ok(())
    }
}

/// Authoritative inline extended-attribute value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XattrRecord {
    inode_id: InodeId,
    name: XattrName,
    value: XattrValue,
    revision: RecordRevision,
}

impl XattrRecord {
    /// Creates one bounded extended-attribute record.
    pub const fn new(
        inode_id: InodeId,
        name: XattrName,
        value: XattrValue,
        revision: RecordRevision,
    ) -> Self {
        Self {
            inode_id,
            name,
            value,
            revision,
        }
    }

    /// Returns the owning inode.
    pub const fn inode_id(&self) -> InodeId {
        self.inode_id
    }

    /// Returns the byte-exact attribute name.
    pub const fn name(&self) -> &XattrName {
        &self.name
    }

    /// Returns the bounded inline value.
    pub const fn value(&self) -> &XattrValue {
        &self.value
    }

    /// Returns the record revision.
    pub const fn revision(&self) -> RecordRevision {
        self.revision
    }
}

/// Portable correctness-bearing staging state for one extended attribute.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XattrStagingRecord {
    staging_id: XattrStagingId,
    inode_id: InodeId,
    name: XattrName,
    expected_size: u64,
    bytes: XattrValue,
    revision: RecordRevision,
}

impl XattrStagingRecord {
    /// Creates bounded staging state, rejecting bytes beyond the declared exact size.
    pub fn new(
        staging_id: XattrStagingId,
        inode_id: InodeId,
        name: XattrName,
        expected_size: u64,
        bytes: XattrValue,
        revision: RecordRevision,
        limits: StateLimits,
    ) -> Result<Self, RecordValidationError> {
        let maximum = u64::try_from(limits.max_xattr_value_bytes()).unwrap_or(u64::MAX);
        if expected_size > maximum {
            return Err(RecordValidationError::XattrExpectedSizeTooLarge {
                expected: expected_size,
                maximum,
            });
        }
        let actual = u64::try_from(bytes.as_bytes().len()).unwrap_or(u64::MAX);
        if actual > expected_size {
            return Err(RecordValidationError::XattrStagingOverflow {
                expected: expected_size,
                actual,
            });
        }
        Ok(Self {
            staging_id,
            inode_id,
            name,
            expected_size,
            bytes,
            revision,
        })
    }

    /// Returns the stable staging identity.
    pub const fn staging_id(&self) -> XattrStagingId {
        self.staging_id
    }

    /// Returns the owning inode.
    pub const fn inode_id(&self) -> InodeId {
        self.inode_id
    }

    /// Returns the future attribute name.
    pub const fn name(&self) -> &XattrName {
        &self.name
    }

    /// Returns the exact byte count required for publication.
    pub const fn expected_size(&self) -> u64 {
        self.expected_size
    }

    /// Returns the currently staged bytes.
    pub const fn bytes(&self) -> &XattrValue {
        &self.bytes
    }

    /// Reports whether staging has reached exactly its declared size.
    pub fn is_complete(&self) -> bool {
        u64::try_from(self.bytes.as_bytes().len()) == Ok(self.expected_size)
    }

    /// Returns the record revision.
    pub const fn revision(&self) -> RecordRevision {
        self.revision
    }
}

/// Durable idempotency-ledger entry committed atomically with filesystem state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MutationRecord {
    filesystem_id: FilesystemId,
    mutation_id: w9pt_fs_storage::MutationId,
    fingerprint: RequestFingerprint,
    client_incarnation: ClientIncarnationId,
    writer_scope: WriterScopeId,
    writer_incarnation: WriterIncarnationId,
    fencing_token: FencingToken,
    result: MutationResult,
    committed_revision: StateRevision,
    retention: MutationRetention,
    revision: RecordRevision,
}

impl MutationRecord {
    /// Creates one exact retained terminal result.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        filesystem_id: FilesystemId,
        mutation_id: w9pt_fs_storage::MutationId,
        fingerprint: RequestFingerprint,
        client_incarnation: ClientIncarnationId,
        writer_scope: WriterScopeId,
        writer_incarnation: WriterIncarnationId,
        fencing_token: FencingToken,
        result: MutationResult,
        committed_revision: StateRevision,
        retention: MutationRetention,
        revision: RecordRevision,
    ) -> Self {
        Self {
            filesystem_id,
            mutation_id,
            fingerprint,
            client_incarnation,
            writer_scope,
            writer_incarnation,
            fencing_token,
            result,
            committed_revision,
            retention,
            revision,
        }
    }

    /// Returns the filesystem authority identity.
    pub const fn filesystem_id(&self) -> FilesystemId {
        self.filesystem_id
    }

    /// Returns the globally stable mutation identity.
    pub const fn mutation_id(&self) -> w9pt_fs_storage::MutationId {
        self.mutation_id
    }

    /// Returns the complete semantic request fingerprint.
    pub const fn fingerprint(&self) -> RequestFingerprint {
        self.fingerprint
    }

    /// Returns the client/session lineage.
    pub const fn client_incarnation(&self) -> ClientIncarnationId {
        self.client_incarnation
    }

    /// Returns the fenced writer scope.
    pub const fn writer_scope(&self) -> WriterScopeId {
        self.writer_scope
    }

    /// Returns the writer incarnation that committed the result.
    pub const fn writer_incarnation(&self) -> WriterIncarnationId {
        self.writer_incarnation
    }

    /// Returns the fencing token accepted for the commit.
    pub const fn fencing_token(&self) -> FencingToken {
        self.fencing_token
    }

    /// Returns the exact retained terminal result.
    pub const fn result(&self) -> &MutationResult {
        &self.result
    }

    /// Returns the all-record state revision created by the commit.
    pub const fn committed_revision(&self) -> StateRevision {
        self.committed_revision
    }

    /// Returns the caller-defined retention horizon.
    pub const fn retention(&self) -> MutationRetention {
        self.retention
    }

    /// Returns the ledger record revision.
    pub const fn revision(&self) -> RecordRevision {
        self.revision
    }
}

/// Current authoritative writer lease for one fenced scope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WriterLeaseRecord {
    filesystem_id: FilesystemId,
    scope: WriterScopeId,
    holder: WriterIncarnationId,
    lease_id: LeaseId,
    deadline: LeaseDeadline,
    fencing_token: FencingToken,
    revision: RecordRevision,
}

impl WriterLeaseRecord {
    /// Creates one granted writer lease.
    pub const fn new(
        filesystem_id: FilesystemId,
        scope: WriterScopeId,
        holder: WriterIncarnationId,
        lease_id: LeaseId,
        deadline: LeaseDeadline,
        fencing_token: FencingToken,
        revision: RecordRevision,
    ) -> Self {
        Self {
            filesystem_id,
            scope,
            holder,
            lease_id,
            deadline,
            fencing_token,
            revision,
        }
    }

    /// Returns the filesystem authority identity.
    pub const fn filesystem_id(&self) -> FilesystemId {
        self.filesystem_id
    }

    /// Returns the fenced writer scope.
    pub const fn scope(&self) -> WriterScopeId {
        self.scope
    }

    /// Returns the holder incarnation.
    pub const fn holder(&self) -> WriterIncarnationId {
        self.holder
    }

    /// Returns the lease identity.
    pub const fn lease_id(&self) -> LeaseId {
        self.lease_id
    }

    /// Returns the adapter-clock deadline.
    pub const fn deadline(&self) -> LeaseDeadline {
        self.deadline
    }

    /// Returns the monotonically allocated fencing token.
    pub const fn fencing_token(&self) -> FencingToken {
        self.fencing_token
    }

    /// Returns the record revision.
    pub const fn revision(&self) -> RecordRevision {
        self.revision
    }
}

impl DirectoryEntryRecord {
    /// Creates a directory entry with a persistent nonzero cookie.
    pub fn new(
        parent_inode_id: InodeId,
        name: EntryName,
        cookie: DirectoryCookie,
        child_inode_id: InodeId,
        revision: RecordRevision,
    ) -> Result<Self, RecordValidationError> {
        if cookie == DirectoryCookie::START {
            return Err(RecordValidationError::ZeroDirectoryCookie);
        }
        Ok(Self {
            parent_inode_id,
            name,
            cookie,
            child_inode_id,
            revision,
        })
    }

    /// Returns the containing directory inode.
    pub const fn parent_inode_id(&self) -> InodeId {
        self.parent_inode_id
    }

    /// Returns the byte-exact component name.
    pub const fn name(&self) -> &EntryName {
        &self.name
    }

    /// Returns the stable enumeration cookie.
    pub const fn cookie(&self) -> DirectoryCookie {
        self.cookie
    }

    /// Returns the stable child inode.
    pub const fn child_inode_id(&self) -> InodeId {
        self.child_inode_id
    }

    /// Returns the record revision.
    pub const fn revision(&self) -> RecordRevision {
        self.revision
    }
}

/// Persistent filesystem object kind, independent of 9P wire mode bits.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum InodeKind {
    /// Regular file with immutable content.
    RegularFile,
    /// Directory containing separately stored entries.
    Directory,
    /// Symbolic link with a bounded target.
    Symlink,
    /// Character device.
    CharacterDevice,
    /// Block device.
    BlockDevice,
    /// Named pipe.
    Fifo,
    /// Unix-domain socket inode.
    Socket,
}

/// Major and minor values for a device inode.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DeviceNumbers {
    /// Device major number.
    pub major: u32,
    /// Device minor number.
    pub minor: u32,
}

/// Kind-specific authoritative inode data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InodeData {
    /// Regular file and its explicit immutable-content identity.
    RegularFile {
        /// File identity used by `w9pt-fs-storage` object preparation.
        content_file_id: w9pt_fs_storage::FileId,
        /// Current prepared content, or `None` before initial content publication.
        content: Option<w9pt_fs_storage::ContentRef>,
        /// Zero before first publication, otherwise equal to `content.generation()`.
        data_generation: u64,
    },
    /// Directory and its namespace generation.
    Directory {
        /// Increases whenever a directory entry changes.
        generation: DirectoryGeneration,
        /// Authoritative parent directory; an export root points to itself.
        parent_inode_id: InodeId,
    },
    /// Symbolic-link target bytes.
    Symlink {
        /// Bounded exact target.
        target: SymlinkTarget,
    },
    /// Character device number.
    CharacterDevice(DeviceNumbers),
    /// Block device number.
    BlockDevice(DeviceNumbers),
    /// Named pipe.
    Fifo,
    /// Unix-domain socket inode.
    Socket,
}

impl InodeData {
    /// Returns the object kind encoded by this variant.
    pub const fn kind(&self) -> InodeKind {
        match self {
            Self::RegularFile { .. } => InodeKind::RegularFile,
            Self::Directory { .. } => InodeKind::Directory,
            Self::Symlink { .. } => InodeKind::Symlink,
            Self::CharacterDevice(_) => InodeKind::CharacterDevice,
            Self::BlockDevice(_) => InodeKind::BlockDevice,
            Self::Fifo => InodeKind::Fifo,
            Self::Socket => InodeKind::Socket,
        }
    }
}

/// Exact caller-supplied inode timestamps.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InodeTimes {
    /// Last access time.
    pub accessed: UnixTimestamp,
    /// Last data modification time.
    pub modified: UnixTimestamp,
    /// Last metadata change time.
    pub changed: UnixTimestamp,
    /// Creation/birth time when known.
    pub created: UnixTimestamp,
}

/// Authoritative inode state at one record revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InodeRecord {
    inode_id: InodeId,
    qid_path: QidPath,
    revision: RecordRevision,
    mode: u32,
    owner: PrincipalId,
    group: GroupId,
    times: InodeTimes,
    logical_size: u64,
    link_count: u64,
    inode_generation: InodeGeneration,
    content_context_id: Option<w9pt_fs_storage::ContentContextId>,
    data: InodeData,
}

impl InodeRecord {
    /// Creates a checked inode record.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        inode_id: InodeId,
        qid_path: QidPath,
        revision: RecordRevision,
        mode: u32,
        owner: PrincipalId,
        group: GroupId,
        times: InodeTimes,
        logical_size: u64,
        link_count: u64,
        inode_generation: InodeGeneration,
        data: InodeData,
    ) -> Result<Self, RecordValidationError> {
        if matches!(data, InodeData::RegularFile { .. }) {
            return Err(RecordValidationError::MissingContentContext);
        }
        Self::new_inner(
            inode_id,
            qid_path,
            revision,
            mode,
            owner,
            group,
            times,
            logical_size,
            link_count,
            inode_generation,
            None,
            data,
        )
    }

    /// Creates a checked regular inode with its mandatory content context.
    #[allow(clippy::too_many_arguments)]
    pub fn new_regular(
        inode_id: InodeId,
        qid_path: QidPath,
        revision: RecordRevision,
        mode: u32,
        owner: PrincipalId,
        group: GroupId,
        times: InodeTimes,
        logical_size: u64,
        link_count: u64,
        inode_generation: InodeGeneration,
        content_context_id: w9pt_fs_storage::ContentContextId,
        data: InodeData,
    ) -> Result<Self, RecordValidationError> {
        if !matches!(data, InodeData::RegularFile { .. }) {
            return Err(RecordValidationError::ContentContextOnNonRegularFile);
        }
        Self::new_inner(
            inode_id,
            qid_path,
            revision,
            mode,
            owner,
            group,
            times,
            logical_size,
            link_count,
            inode_generation,
            Some(content_context_id),
            data,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new_inner(
        inode_id: InodeId,
        qid_path: QidPath,
        revision: RecordRevision,
        mode: u32,
        owner: PrincipalId,
        group: GroupId,
        times: InodeTimes,
        logical_size: u64,
        link_count: u64,
        inode_generation: InodeGeneration,
        content_context_id: Option<w9pt_fs_storage::ContentContextId>,
        data: InodeData,
    ) -> Result<Self, RecordValidationError> {
        if mode & !0o7777 != 0 {
            return Err(RecordValidationError::InvalidMode { mode });
        }
        validate_inode_data(logical_size, &data)?;
        Ok(Self {
            inode_id,
            qid_path,
            revision,
            mode,
            owner,
            group,
            times,
            logical_size,
            link_count,
            inode_generation,
            content_context_id,
            data,
        })
    }

    /// Returns the stable inode identity.
    pub const fn inode_id(&self) -> InodeId {
        self.inode_id
    }

    /// Returns the stable non-reused 9P QID path.
    pub const fn qid_path(&self) -> QidPath {
        self.qid_path
    }

    /// Returns the record revision.
    pub const fn revision(&self) -> RecordRevision {
        self.revision
    }

    /// Returns permission/special mode bits without a duplicated kind tag.
    pub const fn mode(&self) -> u32 {
        self.mode
    }

    /// Returns the owning principal.
    pub const fn owner(&self) -> &PrincipalId {
        &self.owner
    }

    /// Returns the owning group.
    pub const fn group(&self) -> &GroupId {
        &self.group
    }

    /// Returns exact timestamps.
    pub const fn times(&self) -> InodeTimes {
        self.times
    }

    /// Returns authoritative logical size.
    pub const fn logical_size(&self) -> u64 {
        self.logical_size
    }

    /// Returns the current hard-link count; zero is valid for an orphan.
    pub const fn link_count(&self) -> u64 {
        self.link_count
    }

    /// Returns the inode metadata generation.
    pub const fn inode_generation(&self) -> InodeGeneration {
        self.inode_generation
    }

    /// Returns the kind-specific data.
    pub const fn data(&self) -> &InodeData {
        &self.data
    }

    /// Returns the persistent inode kind.
    pub const fn kind(&self) -> InodeKind {
        self.data.kind()
    }

    /// Returns the regular file's explicit immutable-content identity when applicable.
    pub const fn content_file_id(&self) -> Option<w9pt_fs_storage::FileId> {
        match &self.data {
            InodeData::RegularFile {
                content_file_id, ..
            } => Some(*content_file_id),
            _ => None,
        }
    }

    /// Returns the stable regular-file content context when bound.
    pub const fn content_context_id(&self) -> Option<w9pt_fs_storage::ContentContextId> {
        self.content_context_id
    }

    /// Returns the currently published immutable content reference when present.
    pub const fn content(&self) -> Option<&w9pt_fs_storage::ContentRef> {
        match &self.data {
            InodeData::RegularFile { content, .. } => content.as_ref(),
            _ => None,
        }
    }

    /// Returns the exact regular-file content base, including the new-file sentinel.
    pub fn content_base(&self) -> Option<w9pt_fs_storage::BaseContentIdentity> {
        match &self.data {
            InodeData::RegularFile {
                content: Some(content),
                ..
            } => Some(w9pt_fs_storage::BaseContentIdentity::from_content(content)),
            InodeData::RegularFile { content: None, .. } => {
                Some(w9pt_fs_storage::BaseContentIdentity::NEW_FILE)
            }
            _ => None,
        }
    }

    /// Returns the regular-file data generation when applicable.
    pub fn data_generation(&self) -> Option<DataGeneration> {
        match &self.data {
            InodeData::RegularFile {
                data_generation, ..
            } => DataGeneration::new(*data_generation).ok(),
            _ => None,
        }
    }

    /// Returns the directory namespace generation when applicable.
    pub const fn directory_generation(&self) -> Option<DirectoryGeneration> {
        match &self.data {
            InodeData::Directory { generation, .. } => Some(*generation),
            _ => None,
        }
    }

    /// Returns the authoritative parent identity for a directory.
    pub const fn directory_parent(&self) -> Option<InodeId> {
        match &self.data {
            InodeData::Directory {
                parent_inode_id, ..
            } => Some(*parent_inode_id),
            _ => None,
        }
    }

    pub(crate) fn bump_inode_generation(&mut self) -> Result<(), crate::CounterOverflow> {
        self.inode_generation = self.inode_generation.checked_next()?;
        Ok(())
    }

    pub(crate) fn bump_directory_generation(&mut self) -> Result<(), crate::CounterOverflow> {
        let InodeData::Directory { generation, .. } = &mut self.data else {
            return Err(crate::CounterOverflow {
                field: "DirectoryGenerationKind",
            });
        };
        *generation = generation.checked_next()?;
        self.inode_generation = self.inode_generation.checked_next()?;
        Ok(())
    }

    pub(crate) fn adjust_link_count(
        &mut self,
        adjustment: crate::CounterAdjustment,
    ) -> Result<(), crate::CounterOverflow> {
        self.link_count = adjustment
            .apply(self.link_count)
            .ok_or(crate::CounterOverflow { field: "LinkCount" })?;
        self.inode_generation = self.inode_generation.checked_next()?;
        Ok(())
    }

    pub(crate) fn publish_content(&mut self, publication: &crate::PublishContent) {
        let content_file_id = publication.prepared.content().file_id();
        self.logical_size = publication.logical_size;
        self.inode_generation = publication.inode_generation;
        if let Some(mode) = publication.attributes.mode {
            self.mode = mode;
        }
        if let Some(owner) = &publication.attributes.owner {
            self.owner = owner.clone();
        }
        if let Some(group) = &publication.attributes.group {
            self.group = group.clone();
        }
        self.times = publication.attributes.apply_times(self.times);
        self.data = InodeData::RegularFile {
            content_file_id,
            content: Some(publication.prepared.content().clone()),
            data_generation: publication.data_generation.get(),
        };
    }
}

fn validate_inode_data(logical_size: u64, data: &InodeData) -> Result<(), RecordValidationError> {
    match data {
        InodeData::RegularFile {
            content_file_id,
            content: Some(content),
            data_generation,
        } => {
            if content.file_id() != *content_file_id {
                return Err(RecordValidationError::ContentFileMismatch);
            }
            if content.logical_size() != logical_size {
                return Err(RecordValidationError::ContentSizeMismatch {
                    inode_size: logical_size,
                    content_size: content.logical_size(),
                });
            }
            if content.generation() != *data_generation || *data_generation == 0 {
                return Err(RecordValidationError::ContentGenerationMismatch);
            }
        }
        InodeData::RegularFile {
            content: None,
            data_generation,
            ..
        } => {
            if logical_size != 0 || *data_generation != 0 {
                return Err(RecordValidationError::UnpublishedContentState);
            }
        }
        InodeData::Directory { .. }
        | InodeData::CharacterDevice(_)
        | InodeData::BlockDevice(_)
        | InodeData::Fifo
        | InodeData::Socket => {
            if logical_size != 0 {
                return Err(RecordValidationError::NonDataSize {
                    kind: data.kind(),
                    size: logical_size,
                });
            }
        }
        InodeData::Symlink { target } => {
            let target_size = u64::try_from(target.as_bytes().len()).unwrap_or(u64::MAX);
            if logical_size != target_size {
                return Err(RecordValidationError::NonDataSize {
                    kind: InodeKind::Symlink,
                    size: logical_size,
                });
            }
        }
    }
    Ok(())
}

/// Validates exact key/value matching and relationships in a complete authority image.
pub fn validate_record_set(
    records: &BTreeMap<RecordKey, StateRecord>,
) -> Result<(), RecordValidationError> {
    validate_record_set_with_limits(records, StateLimits::default())
}

/// Validates a complete authority image with an explicit ancestry bound.
pub fn validate_record_set_with_limits(
    records: &BTreeMap<RecordKey, StateRecord>,
    limits: StateLimits,
) -> Result<(), RecordValidationError> {
    let mut directory_cookies = BTreeSet::new();
    let mut qid_paths = BTreeSet::new();
    let mut locks = Vec::new();
    let mut content_contexts = BTreeSet::new();
    let mut content_owners = BTreeSet::new();
    for (key, record) in records {
        record.validate_key(key)?;
        let filesystem_id = key.filesystem_id();
        match record {
            StateRecord::Filesystem(filesystem) => {
                let Some(StateRecord::Inode(root)) =
                    records.get(&RecordKey::Inode(filesystem_id, filesystem.root_inode_id()))
                else {
                    return Err(RecordValidationError::MissingRelatedRecord {
                        relation: "filesystem root inode",
                    });
                };
                if root.kind() != InodeKind::Directory {
                    return Err(RecordValidationError::InvalidRelatedRecord {
                        relation: "filesystem root is not a directory",
                    });
                }
                if root.directory_parent() != Some(root.inode_id()) {
                    return Err(RecordValidationError::InvalidRelatedRecord {
                        relation: "filesystem root directory is not self-parented",
                    });
                }
            }
            StateRecord::DirectoryEntry(entry) => {
                let Some(StateRecord::Inode(parent)) =
                    records.get(&RecordKey::Inode(filesystem_id, entry.parent_inode_id()))
                else {
                    return Err(RecordValidationError::MissingRelatedRecord {
                        relation: "directory-entry parent inode",
                    });
                };
                if parent.kind() != InodeKind::Directory {
                    return Err(RecordValidationError::InvalidRelatedRecord {
                        relation: "directory-entry parent is not a directory",
                    });
                }
                if !records.contains_key(&RecordKey::Inode(filesystem_id, entry.child_inode_id())) {
                    return Err(RecordValidationError::MissingRelatedRecord {
                        relation: "directory-entry child inode",
                    });
                }
                let Some(StateRecord::Filesystem(filesystem)) =
                    records.get(&RecordKey::Filesystem(filesystem_id))
                else {
                    return Err(RecordValidationError::MissingRelatedRecord {
                        relation: "directory-entry filesystem header",
                    });
                };
                if entry.cookie() >= filesystem.next_directory_cookie() {
                    return Err(RecordValidationError::InvalidRelatedRecord {
                        relation: "directory-entry cookie was not allocated before next cookie",
                    });
                }
                if !directory_cookies.insert((
                    filesystem_id,
                    entry.parent_inode_id(),
                    entry.cookie(),
                )) {
                    return Err(RecordValidationError::InvalidRelatedRecord {
                        relation: "duplicate directory cookie",
                    });
                }
            }
            StateRecord::Open(open) => {
                require_inode(records, filesystem_id, open.inode_id(), "open inode")?;
            }
            StateRecord::OpenPin(pin) => {
                require_inode(records, filesystem_id, pin.inode_id(), "open-pin inode")?;
                let Some(StateRecord::Open(open)) =
                    records.get(&RecordKey::Open(filesystem_id, pin.open_id()))
                else {
                    return Err(RecordValidationError::MissingRelatedRecord {
                        relation: "open-pin open",
                    });
                };
                if open.inode_id() != pin.inode_id() {
                    return Err(RecordValidationError::InvalidRelatedRecord {
                        relation: "open-pin inode differs from open inode",
                    });
                }
            }
            StateRecord::Orphan(orphan) => {
                let inode =
                    require_inode(records, filesystem_id, orphan.inode_id(), "orphan inode")?;
                if inode.link_count() != 0 {
                    return Err(RecordValidationError::InvalidRelatedRecord {
                        relation: "orphan inode still has links",
                    });
                }
                let pin_count = records
                    .keys()
                    .filter(|key| {
                        matches!(
                            key,
                            RecordKey::OpenPin(fs, inode_id, _)
                                if *fs == filesystem_id && *inode_id == orphan.inode_id()
                        )
                    })
                    .count();
                if u64::try_from(pin_count) != Ok(orphan.open_pin_count()) {
                    return Err(RecordValidationError::InvalidRelatedRecord {
                        relation: "orphan open-pin count mismatch",
                    });
                }
            }
            StateRecord::Lock(lock) => {
                require_inode(records, filesystem_id, lock.inode_id(), "lock inode")?;
                let Some(StateRecord::Open(open)) =
                    records.get(&RecordKey::Open(filesystem_id, lock.owner().open_id()))
                else {
                    return Err(RecordValidationError::MissingRelatedRecord {
                        relation: "lock owner open",
                    });
                };
                if open.inode_id() != lock.inode_id()
                    || open.client_incarnation() != lock.owner().client_incarnation()
                {
                    return Err(RecordValidationError::InvalidRelatedRecord {
                        relation: "lock owner differs from portable open",
                    });
                }
                if locks
                    .iter()
                    .any(|(fs, existing): &(FilesystemId, &LockRecord)| {
                        *fs == filesystem_id && lock.conflicts_with(existing)
                    })
                {
                    return Err(RecordValidationError::InvalidRelatedRecord {
                        relation: "conflicting byte-range locks coexist",
                    });
                }
                locks.push((filesystem_id, lock));
            }
            StateRecord::Xattr(xattr) => {
                require_inode(records, filesystem_id, xattr.inode_id(), "xattr inode")?;
            }
            StateRecord::XattrStaging(staging) => {
                require_inode(
                    records,
                    filesystem_id,
                    staging.inode_id(),
                    "xattr-staging inode",
                )?;
            }
            StateRecord::Inode(inode) => {
                let Some(StateRecord::Filesystem(filesystem)) =
                    records.get(&RecordKey::Filesystem(filesystem_id))
                else {
                    return Err(RecordValidationError::MissingRelatedRecord {
                        relation: "inode filesystem header",
                    });
                };
                if inode.qid_path() >= filesystem.next_qid_path() {
                    return Err(RecordValidationError::InvalidRelatedRecord {
                        relation: "inode QID path was not allocated before next QID path",
                    });
                }
                if !qid_paths.insert((filesystem_id, inode.qid_path())) {
                    return Err(RecordValidationError::InvalidRelatedRecord {
                        relation: "duplicate inode QID path",
                    });
                }
                if let Some(parent_inode_id) = inode.directory_parent()
                    && inode.link_count() != 0
                {
                    let parent = require_inode(
                        records,
                        filesystem_id,
                        parent_inode_id,
                        "directory parent inode",
                    )?;
                    if parent.kind() != InodeKind::Directory {
                        return Err(RecordValidationError::InvalidRelatedRecord {
                            relation: "directory parent is not a directory",
                        });
                    }
                    validate_directory_ancestry(
                        records,
                        filesystem_id,
                        inode.inode_id(),
                        filesystem.root_inode_id(),
                        limits.max_directory_ancestor_depth(),
                    )?;
                }
                if let Some(file_id) = inode.content_file_id() {
                    let context_id = inode.content_context_id().ok_or(
                        RecordValidationError::MissingRelatedRecord {
                            relation: "regular inode content context",
                        },
                    )?;
                    let Some(StateRecord::ContentMetadata(metadata)) =
                        records.get(&RecordKey::ContentMetadata(filesystem_id, file_id))
                    else {
                        return Err(RecordValidationError::MissingRelatedRecord {
                            relation: "regular inode content metadata",
                        });
                    };
                    if metadata.owner_inode_id() != inode.inode_id()
                        || metadata.context_id() != context_id
                    {
                        return Err(RecordValidationError::InvalidRelatedRecord {
                            relation: "inode content context binding",
                        });
                    }
                }
            }
            StateRecord::ContentMetadata(metadata) => {
                if !content_contexts.insert((filesystem_id, metadata.context_id())) {
                    return Err(RecordValidationError::InvalidRelatedRecord {
                        relation: "duplicate content context identity",
                    });
                }
                if !content_owners.insert((filesystem_id, metadata.owner_inode_id())) {
                    return Err(RecordValidationError::InvalidRelatedRecord {
                        relation: "duplicate content metadata owner",
                    });
                }
                if let Some(StateRecord::Inode(owner)) =
                    records.get(&RecordKey::Inode(filesystem_id, metadata.owner_inode_id()))
                    && (owner.kind() != InodeKind::RegularFile
                        || owner.content_file_id() != Some(metadata.content_file_id())
                        || owner.content_context_id() != Some(metadata.context_id()))
                {
                    return Err(RecordValidationError::InvalidRelatedRecord {
                        relation: "content metadata owner binding",
                    });
                }
            }
            StateRecord::Mutation(_) | StateRecord::WriterLease(_) => {}
        }
    }
    for (key, record) in records {
        let (RecordKey::Inode(filesystem_id, inode_id), StateRecord::Inode(inode)) = (key, record)
        else {
            continue;
        };
        let namespace_entries: Vec<_> = records
            .iter()
            .filter_map(|(entry_key, record)| match (entry_key, record) {
                (RecordKey::DirectoryEntry(entry_fs, _, _), StateRecord::DirectoryEntry(entry))
                    if *entry_fs == *filesystem_id && entry.child_inode_id() == *inode_id =>
                {
                    Some(entry)
                }
                _ => None,
            })
            .collect();
        let namespace_links = namespace_entries.len();
        if inode.kind() != InodeKind::Directory {
            if u64::try_from(namespace_links) != Ok(inode.link_count()) {
                return Err(RecordValidationError::InvalidRelatedRecord {
                    relation: "inode link count differs from namespace references",
                });
            }
        } else {
            let Some(StateRecord::Filesystem(filesystem)) =
                records.get(&RecordKey::Filesystem(*filesystem_id))
            else {
                return Err(RecordValidationError::MissingRelatedRecord {
                    relation: "directory filesystem header",
                });
            };
            if *inode_id == filesystem.root_inode_id() {
                if namespace_links != 0 {
                    return Err(RecordValidationError::InvalidRelatedRecord {
                        relation: "filesystem root directory has a namespace hard link",
                    });
                }
            } else if inode.link_count() == 0 {
                if !namespace_entries.is_empty() {
                    return Err(RecordValidationError::InvalidRelatedRecord {
                        relation: "orphan directory still has a namespace entry",
                    });
                }
                if records.keys().any(|key| {
                    matches!(
                        key,
                        RecordKey::DirectoryEntry(entry_fs, parent, _)
                            if entry_fs == filesystem_id && parent == inode_id
                    )
                }) {
                    return Err(RecordValidationError::InvalidRelatedRecord {
                        relation: "orphan directory is not empty",
                    });
                }
            } else {
                if namespace_entries.len() != 1
                    || namespace_entries[0].parent_inode_id() != inode.directory_parent().unwrap()
                {
                    return Err(RecordValidationError::InvalidRelatedRecord {
                        relation: "directory must have exactly one entry in its authoritative parent",
                    });
                }
            }
        }
        if inode.link_count() != 0 {
            continue;
        }
        let pin_count = records
            .keys()
            .filter(|key| {
                matches!(
                    key,
                    RecordKey::OpenPin(fs, candidate, _)
                        if fs == filesystem_id && candidate == inode_id
                )
            })
            .count();
        let orphan = records.get(&RecordKey::Orphan(*filesystem_id, *inode_id));
        if pin_count == 0 {
            return Err(RecordValidationError::InvalidRelatedRecord {
                relation: "unpinned zero-link inode was not retired",
            });
        }
        if !matches!(orphan, Some(StateRecord::Orphan(_))) {
            return Err(RecordValidationError::MissingRelatedRecord {
                relation: "pinned zero-link inode orphan",
            });
        }
    }
    Ok(())
}

fn validate_directory_ancestry(
    records: &BTreeMap<RecordKey, StateRecord>,
    filesystem_id: FilesystemId,
    inode_id: InodeId,
    root_inode_id: InodeId,
    maximum_depth: u32,
) -> Result<(), RecordValidationError> {
    let mut current = inode_id;
    let mut visited = BTreeSet::new();
    // `maximum_depth` bounds parent edges, not visited nodes. Inspecting the
    // root requires one final iteration after traversing exactly that many
    // edges from the directory under validation.
    for _ in 0..=maximum_depth {
        if !visited.insert(current) {
            return Err(RecordValidationError::DirectoryCycle { inode_id });
        }
        let inode = require_inode(records, filesystem_id, current, "directory ancestor inode")?;
        let Some(parent) = inode.directory_parent() else {
            return Err(RecordValidationError::InvalidRelatedRecord {
                relation: "directory ancestor is not a directory",
            });
        };
        if current == root_inode_id {
            return if parent == root_inode_id {
                Ok(())
            } else {
                Err(RecordValidationError::InvalidRelatedRecord {
                    relation: "filesystem root directory is not self-parented",
                })
            };
        }
        current = parent;
    }
    Err(RecordValidationError::DirectoryAncestorLimit {
        maximum: maximum_depth,
    })
}

fn require_inode<'a>(
    records: &'a BTreeMap<RecordKey, StateRecord>,
    filesystem_id: FilesystemId,
    inode_id: InodeId,
    relation: &'static str,
) -> Result<&'a InodeRecord, RecordValidationError> {
    match records.get(&RecordKey::Inode(filesystem_id, inode_id)) {
        Some(StateRecord::Inode(inode)) => Ok(inode),
        _ => Err(RecordValidationError::MissingRelatedRecord { relation }),
    }
}

/// Structurally invalid authoritative record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecordValidationError {
    /// A semantic key and tagged value selected different record families.
    KeyVariantMismatch {
        /// Family selected by the key.
        key: RecordFamily,
        /// Family encoded by the value.
        value: RecordFamily,
    },
    /// A key and value selected the same family but different stable identities.
    KeyIdentityMismatch {
        /// Family whose identities differed.
        family: RecordFamily,
    },
    /// A required relationship target was absent.
    MissingRelatedRecord {
        /// Stable relationship description.
        relation: &'static str,
    },
    /// Present related records violate a cross-record invariant.
    InvalidRelatedRecord {
        /// Stable relationship description.
        relation: &'static str,
    },
    /// Directory ancestry contains a cycle.
    DirectoryCycle {
        /// Directory whose ancestry was validated.
        inode_id: InodeId,
    },
    /// Directory ancestry did not reach the root within the configured bound.
    DirectoryAncestorLimit {
        /// Maximum number of directory-parent edges allowed.
        maximum: u32,
    },
    /// Directory entries and allocation state reserve cookie zero for scan start.
    ZeroDirectoryCookie,
    /// Orphans without links must be retired when no open pins remain.
    ZeroOpenPins,
    /// A finite byte-range endpoint did not follow its start.
    InvalidLockRange {
        /// First byte offset.
        start: u64,
        /// Invalid exclusive endpoint.
        end: u64,
    },
    /// Adding a lock length to its start overflowed.
    LockRangeOverflow {
        /// First byte offset.
        start: u64,
        /// Requested byte length.
        length: u64,
    },
    /// Declared xattr staging length exceeds the configured inline bound.
    XattrExpectedSizeTooLarge {
        /// Declared exact size.
        expected: u64,
        /// Configured maximum.
        maximum: u64,
    },
    /// Staged bytes exceed their declared exact size.
    XattrStagingOverflow {
        /// Declared exact size.
        expected: u64,
        /// Actual staged bytes.
        actual: u64,
    },
    /// A persistent generation was zero.
    ZeroGeneration {
        /// Stable generation domain.
        field: &'static str,
    },
    /// Mode contains bits outside the state model's permission/special mask.
    InvalidMode {
        /// Rejected mode bits.
        mode: u32,
    },
    /// Inode and content references use different stable file identities.
    ContentFileMismatch,
    /// A content context was attached to a non-regular inode.
    ContentContextOnNonRegularFile,
    /// A regular inode omitted its mandatory content context.
    MissingContentContext,
    /// Inode and content logical sizes differ.
    ContentSizeMismatch {
        /// Size stored by the inode.
        inode_size: u64,
        /// Size stored by the content reference.
        content_size: u64,
    },
    /// Data generation does not equal the content generation.
    ContentGenerationMismatch,
    /// A regular file without published content has nonzero size or generation.
    UnpublishedContentState,
    /// A non-regular inode has an inconsistent logical size.
    NonDataSize {
        /// Inode kind.
        kind: InodeKind,
        /// Rejected size.
        size: u64,
    },
}

impl fmt::Display for RecordValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::KeyVariantMismatch { key, value } => {
                write!(
                    formatter,
                    "record key family {key:?} differs from value family {value:?}"
                )
            }
            Self::KeyIdentityMismatch { family } => {
                write!(
                    formatter,
                    "{family:?} record key and value identities differ"
                )
            }
            Self::MissingRelatedRecord { relation } => {
                write!(formatter, "missing related record: {relation}")
            }
            Self::InvalidRelatedRecord { relation } => {
                write!(formatter, "invalid related record: {relation}")
            }
            Self::DirectoryCycle { inode_id } => {
                write!(
                    formatter,
                    "directory ancestry contains a cycle at {inode_id:?}"
                )
            }
            Self::DirectoryAncestorLimit { maximum } => {
                write!(
                    formatter,
                    "directory ancestry exceeds configured depth {maximum}"
                )
            }
            Self::ZeroDirectoryCookie => {
                formatter.write_str("directory cookie zero is reserved for scan start")
            }
            Self::ZeroOpenPins => formatter.write_str("orphan record has no retaining open pins"),
            Self::InvalidLockRange { start, end } => {
                write!(
                    formatter,
                    "lock range endpoint {end} does not follow start {start}"
                )
            }
            Self::LockRangeOverflow { start, length } => {
                write!(
                    formatter,
                    "lock range start {start} plus length {length} overflows"
                )
            }
            Self::XattrExpectedSizeTooLarge { expected, maximum } => write!(
                formatter,
                "xattr staging size {expected} exceeds configured maximum {maximum}"
            ),
            Self::XattrStagingOverflow { expected, actual } => write!(
                formatter,
                "xattr staging has {actual} bytes but declared size is {expected}"
            ),
            Self::ZeroGeneration { field } => write!(formatter, "{field} generation is zero"),
            Self::InvalidMode { mode } => write!(formatter, "invalid inode mode {mode:#o}"),
            Self::ContentFileMismatch => {
                formatter.write_str("inode content file identity mismatch")
            }
            Self::ContentContextOnNonRegularFile => {
                formatter.write_str("content context requires a regular inode")
            }
            Self::MissingContentContext => {
                formatter.write_str("regular inode requires a content context")
            }
            Self::ContentSizeMismatch {
                inode_size,
                content_size,
            } => write!(
                formatter,
                "inode size {inode_size} differs from content size {content_size}"
            ),
            Self::ContentGenerationMismatch => {
                formatter.write_str("inode data generation differs from content generation")
            }
            Self::UnpublishedContentState => {
                formatter.write_str("unpublished regular content has nonzero size or generation")
            }
            Self::NonDataSize { kind, size } => {
                write!(formatter, "{kind:?} inode has invalid logical size {size}")
            }
        }
    }
}

impl std::error::Error for RecordValidationError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GroupId, PrincipalId, StateLimitValues, StateLimits};

    fn identity() -> (PrincipalId, GroupId) {
        let limits = StateLimits::default();
        (
            PrincipalId::new(b"owner".to_vec(), limits).unwrap(),
            GroupId::new(b"group".to_vec(), limits).unwrap(),
        )
    }

    fn times() -> InodeTimes {
        let timestamp = UnixTimestamp::new(1, 2).unwrap();
        InodeTimes {
            accessed: timestamp,
            modified: timestamp,
            changed: timestamp,
            created: timestamp,
        }
    }

    #[test]
    fn unpublished_regular_file_is_explicit_and_empty() {
        let (owner, group) = identity();
        let inode = InodeRecord::new_regular(
            InodeId::from_u128(1),
            QidPath::new(1).unwrap(),
            RecordRevision::new(1).unwrap(),
            0o644,
            owner,
            group,
            times(),
            0,
            1,
            InodeGeneration::new(1).unwrap(),
            w9pt_fs_storage::ContentContextId::from_u128(1),
            InodeData::RegularFile {
                content_file_id: w9pt_fs_storage::FileId::from_u128(1),
                content: None,
                data_generation: 0,
            },
        )
        .unwrap();
        assert_eq!(inode.kind(), InodeKind::RegularFile);
        assert_eq!(inode.logical_size(), 0);
        assert_eq!(inode.data_generation(), None);
    }

    #[test]
    fn mismatched_content_summary_is_rejected() {
        let (owner, group) = identity();
        let content = w9pt_fs_storage::ContentRef::from_persisted(
            w9pt_fs_storage::FileId::from_u128(1),
            1,
            3,
            w9pt_fs_storage::ObjectKey::new("manifest").unwrap(),
            w9pt_fs_storage::Digest::new([1; 32]),
            w9pt_fs_storage::StorageMethod::Raw,
        )
        .unwrap();
        assert!(matches!(
            InodeRecord::new_regular(
                InodeId::from_u128(1),
                QidPath::new(1).unwrap(),
                RecordRevision::new(1).unwrap(),
                0o644,
                owner,
                group,
                times(),
                2,
                1,
                InodeGeneration::new(1).unwrap(),
                w9pt_fs_storage::ContentContextId::from_u128(1),
                InodeData::RegularFile {
                    content_file_id: w9pt_fs_storage::FileId::from_u128(1),
                    content: Some(content),
                    data_generation: 1,
                },
            ),
            Err(RecordValidationError::ContentSizeMismatch { .. })
        ));
    }

    #[test]
    fn directory_cookie_is_persistent_and_nonzero() {
        let limits = StateLimits::default();
        let entry = DirectoryEntryRecord::new(
            InodeId::from_u128(1),
            crate::EntryName::new(b"child".to_vec(), limits).unwrap(),
            DirectoryCookie::new(7),
            InodeId::from_u128(2),
            RecordRevision::new(3).unwrap(),
        )
        .unwrap();
        assert_eq!(entry.cookie().get(), 7);
        assert!(
            DirectoryEntryRecord::new(
                InodeId::from_u128(1),
                crate::EntryName::new(b"other".to_vec(), limits).unwrap(),
                DirectoryCookie::START,
                InodeId::from_u128(3),
                RecordRevision::new(3).unwrap(),
            )
            .is_err()
        );
    }

    #[test]
    fn open_and_orphan_state_is_portable_and_pinned() {
        let inode_id = InodeId::from_u128(1);
        let open_id = OpenId::from_u128(2);
        let open = OpenRecord::new(
            open_id,
            inode_id,
            ClientIncarnationId::from_u128(3),
            OpenAccess::ReadWrite,
            false,
            InodeGeneration::new(4).unwrap(),
            RecordRevision::new(5).unwrap(),
        );
        let pin = OpenPinRecord::new(inode_id, open_id, RecordRevision::new(5).unwrap());
        let orphan = OrphanRecord::new(
            inode_id,
            1,
            StateRevision::new(6).unwrap(),
            RecordRevision::new(6).unwrap(),
        )
        .unwrap();
        assert_eq!(open.open_id(), pin.open_id());
        assert_eq!(orphan.open_pin_count(), 1);
        assert!(
            OrphanRecord::new(
                inode_id,
                0,
                StateRevision::new(6).unwrap(),
                RecordRevision::new(6).unwrap(),
            )
            .is_err()
        );
    }

    #[test]
    fn open_unlinked_directory_needs_pins_but_not_a_namespace_parent() {
        let filesystem_id = FilesystemId::from_u128(10);
        let root_id = InodeId::from_u128(11);
        let directory_id = InodeId::from_u128(12);
        let open_id = OpenId::from_u128(13);
        let revision = RecordRevision::new(1).unwrap();
        let (owner, group) = identity();
        let root = InodeRecord::new(
            root_id,
            QidPath::new(1).unwrap(),
            revision,
            0o755,
            owner.clone(),
            group.clone(),
            times(),
            0,
            1,
            InodeGeneration::new(1).unwrap(),
            InodeData::Directory {
                generation: DirectoryGeneration::new(1).unwrap(),
                parent_inode_id: root_id,
            },
        )
        .unwrap();
        let directory = InodeRecord::new(
            directory_id,
            QidPath::new(2).unwrap(),
            revision,
            0o755,
            owner,
            group,
            times(),
            0,
            0,
            InodeGeneration::new(2).unwrap(),
            InodeData::Directory {
                generation: DirectoryGeneration::new(2).unwrap(),
                parent_inode_id: InodeId::from_u128(99),
            },
        )
        .unwrap();
        let filesystem = FilesystemRecord::new(
            filesystem_id,
            StateRevision::new(1).unwrap(),
            revision,
            root_id,
            QidPath::new(3).unwrap(),
            DirectoryCookie::new(2),
            1,
        )
        .unwrap();
        let open = OpenRecord::new(
            open_id,
            directory_id,
            ClientIncarnationId::from_u128(14),
            OpenAccess::DirectoryRead,
            false,
            directory.inode_generation(),
            revision,
        );
        let pin = OpenPinRecord::new(directory_id, open_id, revision);
        let orphan =
            OrphanRecord::new(directory_id, 1, StateRevision::new(2).unwrap(), revision).unwrap();
        let records = BTreeMap::from([
            (
                RecordKey::Filesystem(filesystem_id),
                StateRecord::Filesystem(filesystem),
            ),
            (
                RecordKey::Inode(filesystem_id, root_id),
                StateRecord::Inode(root),
            ),
            (
                RecordKey::Inode(filesystem_id, directory_id),
                StateRecord::Inode(directory),
            ),
            (
                RecordKey::Open(filesystem_id, open_id),
                StateRecord::Open(open),
            ),
            (
                RecordKey::OpenPin(filesystem_id, directory_id, open_id),
                StateRecord::OpenPin(pin),
            ),
            (
                RecordKey::Orphan(filesystem_id, directory_id),
                StateRecord::Orphan(orphan),
            ),
        ]);
        validate_record_set(&records).unwrap();
    }

    #[test]
    fn lock_ranges_are_checked_and_conflicts_are_deterministic() {
        let finite = LockRange::from_start_and_length(10, 5).unwrap();
        let adjacent = LockRange::finite(15, 20).unwrap();
        let through_eof = LockRange::from_start_and_length(12, 0).unwrap();
        assert_eq!(finite.end(), LockRangeEnd::Exclusive(15));
        assert!(!finite.overlaps(adjacent));
        assert!(finite.overlaps(through_eof));
        assert!(LockRange::from_start_and_length(u64::MAX, 1).is_err());

        let inode = InodeId::from_u128(1);
        let owner_a = LockOwner::new(ClientIncarnationId::from_u128(2), OpenId::from_u128(3));
        let owner_b = LockOwner::new(ClientIncarnationId::from_u128(4), OpenId::from_u128(5));
        let shared = LockRecord::new(
            LockId::from_u128(6),
            inode,
            finite,
            LockKind::Shared,
            owner_a,
            LockGeneration::new(1).unwrap(),
            RecordRevision::new(1).unwrap(),
        );
        let exclusive = LockRecord::new(
            LockId::from_u128(7),
            inode,
            through_eof,
            LockKind::Exclusive,
            owner_b,
            LockGeneration::new(1).unwrap(),
            RecordRevision::new(1).unwrap(),
        );
        assert!(shared.conflicts_with(&exclusive));
        assert!(exclusive.conflicts_with(&shared));
    }

    #[test]
    fn xattr_staging_requires_bounded_exact_publication_size() {
        let limits = StateLimits::default();
        let name = XattrName::new(b"user.mime".to_vec(), limits).unwrap();
        let incomplete = XattrStagingRecord::new(
            XattrStagingId::from_u128(1),
            InodeId::from_u128(2),
            name.clone(),
            3,
            XattrValue::new(b"ab".to_vec(), limits).unwrap(),
            RecordRevision::new(1).unwrap(),
            limits,
        )
        .unwrap();
        assert!(!incomplete.is_complete());
        assert!(
            XattrStagingRecord::new(
                XattrStagingId::from_u128(1),
                InodeId::from_u128(2),
                name,
                1,
                XattrValue::new(b"ab".to_vec(), limits).unwrap(),
                RecordRevision::new(1).unwrap(),
                limits,
            )
            .is_err()
        );
    }

    #[test]
    fn record_keys_require_exact_variants_and_identities() {
        let record = StateRecord::Open(OpenRecord::new(
            OpenId::from_u128(1),
            InodeId::from_u128(2),
            ClientIncarnationId::from_u128(3),
            OpenAccess::ReadOnly,
            false,
            InodeGeneration::new(1).unwrap(),
            RecordRevision::new(1).unwrap(),
        ));
        let filesystem = FilesystemId::from_u128(4);
        assert!(
            record
                .validate_key(&RecordKey::Open(filesystem, OpenId::from_u128(1)))
                .is_ok()
        );
        assert!(matches!(
            record.validate_key(&RecordKey::Open(filesystem, OpenId::from_u128(9))),
            Err(RecordValidationError::KeyIdentityMismatch { .. })
        ));
        assert!(matches!(
            record.validate_key(&RecordKey::Orphan(filesystem, InodeId::from_u128(2))),
            Err(RecordValidationError::KeyVariantMismatch { .. })
        ));
    }

    #[test]
    fn complete_record_sets_validate_root_and_relationships() {
        let limits = StateLimits::default();
        let filesystem_id = FilesystemId::from_u128(1);
        let root_id = InodeId::from_u128(2);
        let revision = RecordRevision::new(1).unwrap();
        let (owner, group) = identity();
        let root = InodeRecord::new(
            root_id,
            QidPath::new(1).unwrap(),
            revision,
            0o755,
            owner,
            group,
            times(),
            0,
            1,
            InodeGeneration::new(1).unwrap(),
            InodeData::Directory {
                generation: DirectoryGeneration::new(1).unwrap(),
                parent_inode_id: root_id,
            },
        )
        .unwrap();
        let filesystem = FilesystemRecord::new(
            filesystem_id,
            StateRevision::new(1).unwrap(),
            revision,
            root_id,
            QidPath::new(2).unwrap(),
            DirectoryCookie::new(1),
            1,
        )
        .unwrap();
        let mut records = BTreeMap::from([
            (
                RecordKey::Filesystem(filesystem_id),
                StateRecord::Filesystem(filesystem),
            ),
            (
                RecordKey::Inode(filesystem_id, root_id),
                StateRecord::Inode(root),
            ),
        ]);
        validate_record_set(&records).unwrap();

        let missing_child = InodeId::from_u128(3);
        let name = EntryName::new(b"missing".to_vec(), limits).unwrap();
        records.insert(
            RecordKey::DirectoryEntry(filesystem_id, root_id, name.clone()),
            StateRecord::DirectoryEntry(
                DirectoryEntryRecord::new(
                    root_id,
                    name,
                    DirectoryCookie::new(2),
                    missing_child,
                    revision,
                )
                .unwrap(),
            ),
        );
        assert!(matches!(
            validate_record_set(&records),
            Err(RecordValidationError::MissingRelatedRecord { .. })
        ));
    }

    #[test]
    fn qid_paths_are_unique_within_each_filesystem() {
        let filesystem_id = FilesystemId::from_u128(10);
        let root_id = InodeId::from_u128(11);
        let duplicate_id = InodeId::from_u128(12);
        let revision = RecordRevision::new(1).unwrap();
        let (owner, group) = identity();
        let root = InodeRecord::new(
            root_id,
            QidPath::new(1).unwrap(),
            revision,
            0o755,
            owner.clone(),
            group.clone(),
            times(),
            0,
            1,
            InodeGeneration::new(1).unwrap(),
            InodeData::Directory {
                generation: DirectoryGeneration::new(1).unwrap(),
                parent_inode_id: root_id,
            },
        )
        .unwrap();
        let duplicate = InodeRecord::new(
            duplicate_id,
            QidPath::new(1).unwrap(),
            revision,
            0o644,
            owner,
            group,
            times(),
            0,
            1,
            InodeGeneration::new(1).unwrap(),
            InodeData::Fifo,
        )
        .unwrap();
        let filesystem = FilesystemRecord::new(
            filesystem_id,
            StateRevision::new(1).unwrap(),
            revision,
            root_id,
            QidPath::new(2).unwrap(),
            DirectoryCookie::new(1),
            1,
        )
        .unwrap();
        let records = BTreeMap::from([
            (
                RecordKey::Filesystem(filesystem_id),
                StateRecord::Filesystem(filesystem),
            ),
            (
                RecordKey::Inode(filesystem_id, root_id),
                StateRecord::Inode(root),
            ),
            (
                RecordKey::Inode(filesystem_id, duplicate_id),
                StateRecord::Inode(duplicate),
            ),
        ]);
        assert!(matches!(
            validate_record_set(&records),
            Err(RecordValidationError::InvalidRelatedRecord {
                relation: "duplicate inode QID path"
            })
        ));
    }

    #[test]
    fn directory_ancestry_accepts_exact_bound_and_rejects_one_edge_beyond() {
        let limits = StateLimits::default();
        let filesystem_id = FilesystemId::from_u128(20);
        let root_id = InodeId::from_u128(21);
        let first_id = InodeId::from_u128(22);
        let second_id = InodeId::from_u128(23);
        let revision = RecordRevision::new(1).unwrap();
        let (owner, group) = identity();
        let directory = |inode_id, qid, parent| {
            InodeRecord::new(
                inode_id,
                QidPath::new(qid).unwrap(),
                revision,
                0o755,
                owner.clone(),
                group.clone(),
                times(),
                0,
                1,
                InodeGeneration::new(1).unwrap(),
                InodeData::Directory {
                    generation: DirectoryGeneration::new(1).unwrap(),
                    parent_inode_id: parent,
                },
            )
            .unwrap()
        };
        let first_name = EntryName::new(b"first".to_vec(), limits).unwrap();
        let second_name = EntryName::new(b"second".to_vec(), limits).unwrap();
        let filesystem = FilesystemRecord::new(
            filesystem_id,
            StateRevision::new(1).unwrap(),
            revision,
            root_id,
            QidPath::new(4).unwrap(),
            DirectoryCookie::new(4),
            1,
        )
        .unwrap();
        let records = BTreeMap::from([
            (
                RecordKey::Filesystem(filesystem_id),
                StateRecord::Filesystem(filesystem),
            ),
            (
                RecordKey::Inode(filesystem_id, root_id),
                StateRecord::Inode(directory(root_id, 1, root_id)),
            ),
            (
                RecordKey::Inode(filesystem_id, first_id),
                StateRecord::Inode(directory(first_id, 2, root_id)),
            ),
            (
                RecordKey::Inode(filesystem_id, second_id),
                StateRecord::Inode(directory(second_id, 3, first_id)),
            ),
            (
                RecordKey::DirectoryEntry(filesystem_id, root_id, first_name.clone()),
                StateRecord::DirectoryEntry(
                    DirectoryEntryRecord::new(
                        root_id,
                        first_name.clone(),
                        DirectoryCookie::new(1),
                        first_id,
                        revision,
                    )
                    .unwrap(),
                ),
            ),
            (
                RecordKey::DirectoryEntry(filesystem_id, first_id, second_name.clone()),
                StateRecord::DirectoryEntry(
                    DirectoryEntryRecord::new(
                        first_id,
                        second_name,
                        DirectoryCookie::new(2),
                        second_id,
                        revision,
                    )
                    .unwrap(),
                ),
            ),
        ]);
        validate_record_set(&records).unwrap();

        let mut hard_linked = records.clone();
        let alias = EntryName::new(b"alias".to_vec(), limits).unwrap();
        hard_linked.insert(
            RecordKey::DirectoryEntry(filesystem_id, root_id, alias.clone()),
            StateRecord::DirectoryEntry(
                DirectoryEntryRecord::new(
                    root_id,
                    alias,
                    DirectoryCookie::new(3),
                    second_id,
                    revision,
                )
                .unwrap(),
            ),
        );
        assert!(matches!(
            validate_record_set(&hard_linked),
            Err(RecordValidationError::InvalidRelatedRecord {
                relation: "directory must have exactly one entry in its authoritative parent"
            })
        ));

        let mut cyclic = records.clone();
        cyclic.remove(&RecordKey::DirectoryEntry(
            filesystem_id,
            root_id,
            first_name.clone(),
        ));
        cyclic.insert(
            RecordKey::DirectoryEntry(filesystem_id, second_id, first_name.clone()),
            StateRecord::DirectoryEntry(
                DirectoryEntryRecord::new(
                    second_id,
                    first_name,
                    DirectoryCookie::new(1),
                    first_id,
                    revision,
                )
                .unwrap(),
            ),
        );
        cyclic.insert(
            RecordKey::Inode(filesystem_id, first_id),
            StateRecord::Inode(directory(first_id, 2, second_id)),
        );
        assert!(matches!(
            validate_record_set(&cyclic),
            Err(RecordValidationError::DirectoryCycle { .. })
        ));

        let exact_bound = StateLimits::new(StateLimitValues {
            max_directory_ancestor_depth: 2,
            ..StateLimitValues::default()
        })
        .unwrap();
        validate_record_set_with_limits(&records, exact_bound).unwrap();

        let one_edge_short = StateLimits::new(StateLimitValues {
            max_directory_ancestor_depth: 1,
            ..StateLimitValues::default()
        })
        .unwrap();
        assert_eq!(
            validate_record_set_with_limits(&records, one_edge_short),
            Err(RecordValidationError::DirectoryAncestorLimit { maximum: 1 })
        );
    }
}
