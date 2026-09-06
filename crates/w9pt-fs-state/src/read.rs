//! Typed bounded point and ordered-range read requests.

use core::fmt;

use crate::{
    DirectoryCookie, DirectoryEntryRecord, EntryName, FilesystemId, InodeId, InodeKind,
    InodeRecord, LockId, OpenId, QidPath, RecordFamily, RecordKey, RecordRevision,
    RecordValidationError, StateLimitError, StateLimitKind, StateLimits, StateRecord,
    StateRevision, WriterScopeId, XattrName, XattrStagingId,
};

/// Freshness requirement for one linearizable read batch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadConsistency {
    /// Read the latest authoritative revision available to the adapter.
    LatestLinearizable,
    /// Return this revision or a newer linearizable revision; not historical MVCC.
    AtLeast(StateRevision),
}

/// Validated owned batch of queries served from one authority revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadBatch {
    filesystem_id: FilesystemId,
    consistency: ReadConsistency,
    queries: Box<[ReadQuery]>,
}

impl ReadBatch {
    /// Creates a query batch within the configured count bound.
    pub fn new(
        filesystem_id: FilesystemId,
        consistency: ReadConsistency,
        queries: impl Into<Vec<ReadQuery>>,
        limits: StateLimits,
    ) -> Result<Self, StateLimitError> {
        let queries = queries.into();
        limits.require_count(
            StateLimitKind::ReadQueries,
            queries.len(),
            limits.max_read_queries(),
        )?;
        Ok(Self {
            filesystem_id,
            consistency,
            queries: queries.into_boxed_slice(),
        })
    }

    /// Returns the selected filesystem authority.
    pub const fn filesystem_id(&self) -> FilesystemId {
        self.filesystem_id
    }

    /// Returns the requested freshness contract.
    pub const fn consistency(&self) -> ReadConsistency {
        self.consistency
    }

    /// Returns positionally ordered queries.
    pub fn queries(&self) -> &[ReadQuery] {
        &self.queries
    }

    pub(crate) fn validate(&self, limits: StateLimits) -> Result<(), StateLimitError> {
        limits.require_count(
            StateLimitKind::ReadQueries,
            self.queries.len(),
            limits.max_read_queries(),
        )?;
        for query in &self.queries {
            if let Some(key) = query.point_key(self.filesystem_id) {
                key.validate_against_limits(limits)?;
            }
            let bounds = match query {
                ReadQuery::Scan(scan) => Some(scan.bounds()),
                ReadQuery::DirectoryPage { bounds, .. } => Some(*bounds),
                _ => None,
            };
            if let Some(bounds) = bounds {
                bounds.validate(limits).map_err(|error| match error {
                    InvalidScanBounds::Limit(error) => error,
                    InvalidScanBounds::ZeroItems => StateLimitError::new(
                        StateLimitKind::ScanItems,
                        0,
                        u64::from(limits.max_scan_items()),
                    ),
                    InvalidScanBounds::ZeroBytes => StateLimitError::new(
                        StateLimitKind::ScanBytes,
                        0,
                        u64::try_from(limits.max_scan_bytes()).unwrap_or(u64::MAX),
                    ),
                })?;
                if let ReadQuery::Scan(RecordScan::Xattrs {
                    after: Some(cursor),
                    ..
                }) = query
                {
                    limits.require_bytes(
                        StateLimitKind::XattrName,
                        cursor.name.as_bytes().len(),
                        limits.max_xattr_name_bytes(),
                    )?;
                }
            }
        }
        Ok(())
    }
}

/// Bounds applied before an adapter materializes an ordered scan page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScanBounds {
    max_items: u32,
    max_bytes: usize,
}

impl ScanBounds {
    /// Creates nonzero scan bounds no larger than the store contract permits.
    pub fn new(
        max_items: u32,
        max_bytes: usize,
        limits: StateLimits,
    ) -> Result<Self, InvalidScanBounds> {
        if max_items == 0 {
            return Err(InvalidScanBounds::ZeroItems);
        }
        if max_bytes == 0 {
            return Err(InvalidScanBounds::ZeroBytes);
        }
        limits
            .require_count(
                StateLimitKind::ScanItems,
                usize::try_from(max_items).unwrap_or(usize::MAX),
                limits.max_scan_items(),
            )
            .map_err(InvalidScanBounds::Limit)?;
        limits
            .require_bytes(
                StateLimitKind::ScanBytes,
                max_bytes,
                limits.max_scan_bytes(),
            )
            .map_err(InvalidScanBounds::Limit)?;
        Ok(Self {
            max_items,
            max_bytes,
        })
    }

    /// Returns the maximum records in the page.
    pub const fn max_items(self) -> u32 {
        self.max_items
    }

    /// Returns the maximum estimated encoded bytes in the page.
    pub const fn max_bytes(self) -> usize {
        self.max_bytes
    }

    pub(crate) fn validate(self, limits: StateLimits) -> Result<(), InvalidScanBounds> {
        Self::new(self.max_items, self.max_bytes, limits).map(|_| ())
    }
}

/// Invalid caller-supplied scan bounds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidScanBounds {
    /// A scan must permit at least one item.
    ZeroItems,
    /// A scan must permit at least one byte.
    ZeroBytes,
    /// A bound exceeds the store contract.
    Limit(StateLimitError),
}

impl fmt::Display for InvalidScanBounds {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroItems => formatter.write_str("scan item bound must be nonzero"),
            Self::ZeroBytes => formatter.write_str("scan byte bound must be nonzero"),
            Self::Limit(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for InvalidScanBounds {}

/// Stable resume point for an open-pin scan.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct OpenPinCursor {
    /// Last inode returned.
    pub inode_id: InodeId,
    /// Last open returned within that inode.
    pub open_id: OpenId,
}

/// Stable resume point for a lock scan.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LockCursor {
    /// Last inode returned.
    pub inode_id: InodeId,
    /// Last lock returned within that inode.
    pub lock_id: LockId,
}

/// Stable resume point for an xattr scan.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct XattrCursor {
    /// Last inode returned.
    pub inode_id: InodeId,
    /// Last byte-exact xattr name returned within that inode.
    pub name: XattrName,
}

/// Stable cursor returned when another ordered scan page is available.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ScanResume {
    /// Last inode returned.
    Inode(InodeId),
    /// Last content file identity returned.
    ContentMetadata(w9pt_fs_storage::FileId),
    /// Last directory cookie returned.
    DirectoryEntry(DirectoryCookie),
    /// Last open returned.
    Open(OpenId),
    /// Last open pin returned.
    OpenPin(OpenPinCursor),
    /// Last orphan inode returned.
    Orphan(InodeId),
    /// Last lock returned.
    Lock(LockCursor),
    /// Last xattr returned.
    Xattr(XattrCursor),
    /// Last xattr staging identity returned.
    XattrStaging(XattrStagingId),
    /// Last mutation identity returned.
    Mutation(w9pt_fs_storage::MutationId),
    /// Last writer scope returned.
    WriterLease(WriterScopeId),
}

impl ScanResume {
    /// Returns the record family selected by this cursor.
    pub const fn family(&self) -> RecordFamily {
        match self {
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
}

/// Typed bounded scan over one authoritative record family.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecordScan {
    /// Inodes in stable identity order.
    Inodes {
        /// Exclusive resume identity.
        after: Option<InodeId>,
        /// Page bounds.
        bounds: ScanBounds,
    },
    /// Content metadata in stable content-file identity order.
    ContentMetadata {
        /// Exclusive resume identity.
        after: Option<w9pt_fs_storage::FileId>,
        /// Page bounds.
        bounds: ScanBounds,
    },
    /// One directory in stable cookie order.
    DirectoryEntries {
        /// Directory being enumerated.
        parent_inode_id: InodeId,
        /// Exclusive resume cookie; zero starts the scan.
        after: DirectoryCookie,
        /// Page bounds.
        bounds: ScanBounds,
    },
    /// Opens in stable open-identity order.
    Opens {
        /// Exclusive resume identity.
        after: Option<OpenId>,
        /// Page bounds.
        bounds: ScanBounds,
    },
    /// Open pins in `(inode, open)` order.
    OpenPins {
        /// Exclusive resume tuple.
        after: Option<OpenPinCursor>,
        /// Page bounds.
        bounds: ScanBounds,
    },
    /// Orphans in stable inode order.
    Orphans {
        /// Exclusive resume identity.
        after: Option<InodeId>,
        /// Page bounds.
        bounds: ScanBounds,
    },
    /// Locks in `(inode, lock)` order.
    Locks {
        /// Exclusive resume tuple.
        after: Option<LockCursor>,
        /// Page bounds.
        bounds: ScanBounds,
    },
    /// Xattrs in `(inode, name-bytes)` order.
    Xattrs {
        /// Exclusive resume tuple.
        after: Option<XattrCursor>,
        /// Page bounds.
        bounds: ScanBounds,
    },
    /// Xattr staging records in stable identity order.
    XattrStaging {
        /// Exclusive resume identity.
        after: Option<XattrStagingId>,
        /// Page bounds.
        bounds: ScanBounds,
    },
    /// Mutation ledger records in stable mutation-ID order.
    Mutations {
        /// Exclusive resume identity.
        after: Option<w9pt_fs_storage::MutationId>,
        /// Page bounds.
        bounds: ScanBounds,
    },
    /// Writer leases in stable scope order.
    WriterLeases {
        /// Exclusive resume scope.
        after: Option<WriterScopeId>,
        /// Page bounds.
        bounds: ScanBounds,
    },
}

impl RecordScan {
    /// Returns the scanned record family.
    pub const fn family(&self) -> RecordFamily {
        match self {
            Self::Inodes { .. } => RecordFamily::Inode,
            Self::ContentMetadata { .. } => RecordFamily::ContentMetadata,
            Self::DirectoryEntries { .. } => RecordFamily::DirectoryEntry,
            Self::Opens { .. } => RecordFamily::Open,
            Self::OpenPins { .. } => RecordFamily::OpenPin,
            Self::Orphans { .. } => RecordFamily::Orphan,
            Self::Locks { .. } => RecordFamily::Lock,
            Self::Xattrs { .. } => RecordFamily::Xattr,
            Self::XattrStaging { .. } => RecordFamily::XattrStaging,
            Self::Mutations { .. } => RecordFamily::Mutation,
            Self::WriterLeases { .. } => RecordFamily::WriterLease,
        }
    }

    /// Returns this scan's validated page bounds.
    pub const fn bounds(&self) -> ScanBounds {
        match self {
            Self::Inodes { bounds, .. }
            | Self::ContentMetadata { bounds, .. }
            | Self::DirectoryEntries { bounds, .. }
            | Self::Opens { bounds, .. }
            | Self::OpenPins { bounds, .. }
            | Self::Orphans { bounds, .. }
            | Self::Locks { bounds, .. }
            | Self::Xattrs { bounds, .. }
            | Self::XattrStaging { bounds, .. }
            | Self::Mutations { bounds, .. }
            | Self::WriterLeases { bounds, .. } => *bounds,
        }
    }
}

/// Typed point read or bounded ordered scan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReadQuery {
    /// Read the filesystem header.
    Filesystem,
    /// Read one inode.
    Inode(InodeId),
    /// Read one content metadata record by storage file identity.
    ContentMetadata(w9pt_fs_storage::FileId),
    /// Read a regular inode and its selected context from one snapshot.
    InodeWithContentMetadata(InodeId),
    /// Read one inode by its per-filesystem stable QID path.
    InodeByQidPath(QidPath),
    /// Read one directory component.
    DirectoryEntry {
        /// Parent directory.
        parent_inode_id: InodeId,
        /// Byte-exact component.
        name: EntryName,
    },
    /// Read one semantic directory page with child kind and QID summaries.
    DirectoryPage {
        /// Directory being enumerated.
        parent_inode_id: InodeId,
        /// Exclusive persistent cookie; zero starts enumeration.
        after: DirectoryCookie,
        /// Complete-entry item and byte bounds.
        bounds: ScanBounds,
    },
    /// Read one portable open.
    Open(OpenId),
    /// Read one durable open pin.
    OpenPin {
        /// Pinned inode.
        inode_id: InodeId,
        /// Pinning open.
        open_id: OpenId,
    },
    /// Count authoritative open pins for one inode without scanning them.
    OpenPinCount(InodeId),
    /// Read one orphan.
    Orphan(InodeId),
    /// Read one byte-range lock.
    Lock {
        /// Locked inode.
        inode_id: InodeId,
        /// Stable lock identity.
        lock_id: LockId,
    },
    /// Read one xattr.
    Xattr {
        /// Owning inode.
        inode_id: InodeId,
        /// Byte-exact xattr name.
        name: XattrName,
    },
    /// Read one xattr staging record.
    XattrStaging(XattrStagingId),
    /// Read one mutation result.
    Mutation(w9pt_fs_storage::MutationId),
    /// Read the current writer lease for one scope.
    WriterLease(WriterScopeId),
    /// Execute a bounded ordered family scan.
    Scan(RecordScan),
}

impl ReadQuery {
    /// Converts a primary-key point query to its semantic key.
    ///
    /// Secondary QID-path lookup and scans return `None`.
    pub fn point_key(&self, filesystem_id: crate::FilesystemId) -> Option<RecordKey> {
        match self {
            Self::Filesystem => Some(RecordKey::Filesystem(filesystem_id)),
            Self::Inode(inode_id) => Some(RecordKey::Inode(filesystem_id, *inode_id)),
            Self::ContentMetadata(file_id) => {
                Some(RecordKey::ContentMetadata(filesystem_id, *file_id))
            }
            Self::InodeWithContentMetadata(_) => None,
            Self::InodeByQidPath(_) => None,
            Self::DirectoryEntry {
                parent_inode_id,
                name,
            } => Some(RecordKey::DirectoryEntry(
                filesystem_id,
                *parent_inode_id,
                name.clone(),
            )),
            Self::DirectoryPage { .. } => None,
            Self::Open(open_id) => Some(RecordKey::Open(filesystem_id, *open_id)),
            Self::OpenPin { inode_id, open_id } => {
                Some(RecordKey::OpenPin(filesystem_id, *inode_id, *open_id))
            }
            Self::OpenPinCount(_) => None,
            Self::Orphan(inode_id) => Some(RecordKey::Orphan(filesystem_id, *inode_id)),
            Self::Lock { inode_id, lock_id } => {
                Some(RecordKey::Lock(filesystem_id, *inode_id, *lock_id))
            }
            Self::Xattr { inode_id, name } => {
                Some(RecordKey::Xattr(filesystem_id, *inode_id, name.clone()))
            }
            Self::XattrStaging(staging_id) => {
                Some(RecordKey::XattrStaging(filesystem_id, *staging_id))
            }
            Self::Mutation(mutation_id) => Some(RecordKey::Mutation(filesystem_id, *mutation_id)),
            Self::WriterLease(scope) => Some(RecordKey::WriterLease(filesystem_id, *scope)),
            Self::Scan(_) => None,
        }
    }
}

/// One directory entry joined to immutable child identity data at one revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectoryPageEntry {
    entry: DirectoryEntryRecord,
    child_kind: InodeKind,
    child_qid_path: QidPath,
    child_record_revision: RecordRevision,
}

impl DirectoryPageEntry {
    /// Constructs a checked semantic entry from one dentry and child summary.
    pub const fn new(
        entry: DirectoryEntryRecord,
        child_kind: InodeKind,
        child_qid_path: QidPath,
        child_record_revision: RecordRevision,
    ) -> Self {
        Self {
            entry,
            child_kind,
            child_qid_path,
            child_record_revision,
        }
    }

    /// Returns the authoritative directory-entry record.
    pub const fn entry(&self) -> &DirectoryEntryRecord {
        &self.entry
    }

    /// Returns the child inode kind observed in the same snapshot.
    pub const fn child_kind(&self) -> InodeKind {
        self.child_kind
    }

    /// Returns the child's stable QID path observed in the same snapshot.
    pub const fn child_qid_path(&self) -> QidPath {
        self.child_qid_path
    }

    /// Returns the child inode record revision observed in the same snapshot.
    pub const fn child_record_revision(&self) -> RecordRevision {
        self.child_record_revision
    }

    pub(crate) fn retained_bytes(&self) -> Option<usize> {
        self.entry
            .name()
            .as_bytes()
            .len()
            .checked_mul(2)?
            .checked_add(192)
    }
}

/// Bounded complete semantic directory entries in persistent cookie order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectoryPage {
    entries: Box<[DirectoryPageEntry]>,
    resume: Option<DirectoryCookie>,
}

impl DirectoryPage {
    /// Constructs a semantic directory page for adapter-side validation.
    pub fn new(
        entries: impl Into<Vec<DirectoryPageEntry>>,
        resume: Option<DirectoryCookie>,
    ) -> Self {
        Self {
            entries: entries.into().into_boxed_slice(),
            resume,
        }
    }

    /// Returns complete ordered semantic entries.
    pub fn entries(&self) -> &[DirectoryPageEntry] {
        &self.entries
    }

    /// Returns the last emitted cookie when another page is available.
    pub const fn resume(&self) -> Option<DirectoryCookie> {
        self.resume
    }

    fn validate_against(
        &self,
        parent_inode_id: InodeId,
        after: DirectoryCookie,
        bounds: ScanBounds,
        snapshot_revision: StateRevision,
    ) -> Result<(), InvalidStateSnapshot> {
        if self.entries.len() > usize::try_from(bounds.max_items()).unwrap_or(usize::MAX) {
            return Err(InvalidStateSnapshot::ScanItemLimit);
        }
        let mut previous = after;
        let mut bytes = 0usize;
        for entry in &self.entries {
            if entry.entry().parent_inode_id() != parent_inode_id
                || entry.entry().cookie() <= previous
            {
                return Err(InvalidStateSnapshot::RecordOutsideScan);
            }
            if entry.entry().revision().get() > snapshot_revision.get()
                || entry.child_record_revision().get() > snapshot_revision.get()
            {
                return Err(InvalidStateSnapshot::RecordRevisionAfterSnapshot);
            }
            previous = entry.entry().cookie();
            bytes = bytes
                .checked_add(
                    entry
                        .retained_bytes()
                        .ok_or(InvalidStateSnapshot::ScanByteLimit)?,
                )
                .ok_or(InvalidStateSnapshot::ScanByteLimit)?;
            if bytes > bounds.max_bytes() {
                return Err(InvalidStateSnapshot::ScanByteLimit);
            }
        }
        if let Some(resume) = self.resume
            && (self.entries.is_empty() || resume != previous)
        {
            return Err(InvalidStateSnapshot::InvalidResumeCursor);
        }
        Ok(())
    }
}

/// Bounded ordered scan result from one state revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScanPage {
    family: RecordFamily,
    records: Box<[(RecordKey, StateRecord)]>,
    resume: Option<ScanResume>,
}

impl ScanPage {
    /// Creates a scan page and validates every tagged key and cursor family.
    pub fn new(
        family: RecordFamily,
        records: impl Into<Vec<(RecordKey, StateRecord)>>,
        resume: Option<ScanResume>,
    ) -> Result<Self, InvalidStateSnapshot> {
        let records = records.into();
        for (key, record) in &records {
            if key.family() != family {
                return Err(InvalidStateSnapshot::UnexpectedRecordFamily {
                    expected: family,
                    actual: key.family(),
                });
            }
            record
                .validate_key(key)
                .map_err(InvalidStateSnapshot::InvalidRecord)?;
        }
        if let Some(cursor) = &resume
            && cursor.family() != family
        {
            return Err(InvalidStateSnapshot::UnexpectedRecordFamily {
                expected: family,
                actual: cursor.family(),
            });
        }
        Ok(Self {
            family,
            records: records.into_boxed_slice(),
            resume,
        })
    }

    /// Returns the scanned record family.
    pub const fn family(&self) -> RecordFamily {
        self.family
    }

    /// Returns ordered key/value pairs.
    pub fn records(&self) -> &[(RecordKey, StateRecord)] {
        &self.records
    }

    /// Returns a stable exclusive cursor when another page may exist.
    pub const fn resume(&self) -> Option<&ScanResume> {
        self.resume.as_ref()
    }

    fn validate_against(
        &self,
        filesystem_id: FilesystemId,
        scan: &RecordScan,
        snapshot_revision: StateRevision,
    ) -> Result<(), InvalidStateSnapshot> {
        if self.family != scan.family() {
            return Err(InvalidStateSnapshot::UnexpectedRecordFamily {
                expected: scan.family(),
                actual: self.family,
            });
        }
        let bounds = scan.bounds();
        if self.records.len() > usize::try_from(bounds.max_items()).unwrap_or(usize::MAX) {
            return Err(InvalidStateSnapshot::ScanItemLimit);
        }
        let mut bytes = 0usize;
        let mut previous = None;
        for (key, record) in &self.records {
            if key.filesystem_id() != filesystem_id {
                return Err(InvalidStateSnapshot::ScanOutsideFilesystem);
            }
            if record.revision().get() > snapshot_revision.get() {
                return Err(InvalidStateSnapshot::RecordRevisionAfterSnapshot);
            }
            let cursor =
                scan_cursor(scan, key, record).ok_or(InvalidStateSnapshot::RecordOutsideScan)?;
            if previous.as_ref().is_some_and(|prior| prior >= &cursor) {
                return Err(InvalidStateSnapshot::UnorderedScan);
            }
            previous = Some(cursor);
            let item_bytes = key
                .retained_bytes()
                .and_then(|size| size.checked_add(record.retained_bytes()?))
                .ok_or(InvalidStateSnapshot::ScanByteLimit)?;
            bytes = bytes
                .checked_add(item_bytes)
                .ok_or(InvalidStateSnapshot::ScanByteLimit)?;
            if bytes > bounds.max_bytes() {
                return Err(InvalidStateSnapshot::ScanByteLimit);
            }
        }
        if let Some(resume) = &self.resume
            && previous.as_ref() != Some(resume)
        {
            return Err(InvalidStateSnapshot::InvalidResumeCursor);
        }
        Ok(())
    }
}

fn scan_cursor(scan: &RecordScan, key: &RecordKey, record: &StateRecord) -> Option<ScanResume> {
    match (scan, key, record) {
        (
            RecordScan::Inodes { after, .. },
            RecordKey::Inode(_, inode_id),
            StateRecord::Inode(_),
        ) if after.is_none_or(|cursor| *inode_id > cursor) => Some(ScanResume::Inode(*inode_id)),
        (
            RecordScan::ContentMetadata { after, .. },
            RecordKey::ContentMetadata(_, file_id),
            StateRecord::ContentMetadata(_),
        ) if after.is_none_or(|cursor| *file_id > cursor) => {
            Some(ScanResume::ContentMetadata(*file_id))
        }
        (
            RecordScan::DirectoryEntries {
                parent_inode_id,
                after,
                ..
            },
            RecordKey::DirectoryEntry(_, parent, _),
            StateRecord::DirectoryEntry(entry),
        ) if parent == parent_inode_id && entry.cookie() > *after => {
            Some(ScanResume::DirectoryEntry(entry.cookie()))
        }
        (RecordScan::Opens { after, .. }, RecordKey::Open(_, open_id), StateRecord::Open(_))
            if after.is_none_or(|cursor| *open_id > cursor) =>
        {
            Some(ScanResume::Open(*open_id))
        }
        (
            RecordScan::OpenPins { after, .. },
            RecordKey::OpenPin(_, inode_id, open_id),
            StateRecord::OpenPin(_),
        ) if after
            .is_none_or(|cursor| (*inode_id, *open_id) > (cursor.inode_id, cursor.open_id)) =>
        {
            Some(ScanResume::OpenPin(OpenPinCursor {
                inode_id: *inode_id,
                open_id: *open_id,
            }))
        }
        (
            RecordScan::Orphans { after, .. },
            RecordKey::Orphan(_, inode_id),
            StateRecord::Orphan(_),
        ) if after.is_none_or(|cursor| *inode_id > cursor) => Some(ScanResume::Orphan(*inode_id)),
        (
            RecordScan::Locks { after, .. },
            RecordKey::Lock(_, inode_id, lock_id),
            StateRecord::Lock(_),
        ) if after
            .is_none_or(|cursor| (*inode_id, *lock_id) > (cursor.inode_id, cursor.lock_id)) =>
        {
            Some(ScanResume::Lock(LockCursor {
                inode_id: *inode_id,
                lock_id: *lock_id,
            }))
        }
        (
            RecordScan::Xattrs { after, .. },
            RecordKey::Xattr(_, inode_id, name),
            StateRecord::Xattr(_),
        ) if after
            .as_ref()
            .is_none_or(|cursor| (*inode_id, name) > (cursor.inode_id, &cursor.name)) =>
        {
            Some(ScanResume::Xattr(XattrCursor {
                inode_id: *inode_id,
                name: name.clone(),
            }))
        }
        (
            RecordScan::XattrStaging { after, .. },
            RecordKey::XattrStaging(_, staging_id),
            StateRecord::XattrStaging(_),
        ) if after.is_none_or(|cursor| *staging_id > cursor) => {
            Some(ScanResume::XattrStaging(*staging_id))
        }
        (
            RecordScan::Mutations { after, .. },
            RecordKey::Mutation(_, mutation_id),
            StateRecord::Mutation(_),
        ) if after.is_none_or(|cursor| *mutation_id > cursor) => {
            Some(ScanResume::Mutation(*mutation_id))
        }
        (
            RecordScan::WriterLeases { after, .. },
            RecordKey::WriterLease(_, scope),
            StateRecord::WriterLease(_),
        ) if after.is_none_or(|cursor| *scope > cursor) => Some(ScanResume::WriterLease(*scope)),
        _ => None,
    }
}

/// Result position corresponding exactly to one [`ReadQuery`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReadResult {
    /// Point lookup with its semantic key and optional value.
    Point {
        /// Exact queried key.
        key: RecordKey,
        /// Present checked value, or `None` when absent.
        record: Option<Box<StateRecord>>,
    },
    /// Secondary lookup of an inode by its persisted QID path.
    InodeByQidPath {
        /// Exact queried QID path.
        qid_path: QidPath,
        /// Present checked inode, or `None` when absent in this filesystem.
        inode: Option<Box<InodeRecord>>,
    },
    /// Composite regular inode/context result from one authoritative snapshot.
    InodeWithContentMetadata {
        /// Exact requested inode identity.
        inode_id: InodeId,
        /// Present inode, or `None` when absent.
        inode: Option<Box<InodeRecord>>,
        /// Matching selected context, absent with a missing/unbound inode.
        metadata: Option<Box<crate::ContentMetadataRecord>>,
    },
    /// One-revision semantic directory page.
    DirectoryPage(DirectoryPage),
    /// Fixed-size authoritative open-pin count.
    OpenPinCount {
        /// Exact queried inode.
        inode_id: InodeId,
        /// Number of matching durable pins.
        count: u64,
    },
    /// Bounded ordered scan page.
    Scan(ScanPage),
}

/// One-revision result for an entire read batch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateSnapshot {
    revision: StateRevision,
    results: Box<[ReadResult]>,
}

/// Semantic outcome of a consistent read request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReadOutcome {
    /// The requested freshness floor was satisfied.
    Snapshot(StateSnapshot),
    /// The caller requested a revision newer than the authority currently has.
    RevisionUnavailable {
        /// Requested minimum revision.
        required: StateRevision,
        /// Current authoritative revision.
        current: StateRevision,
    },
    /// Request was constructed against looser limits than this adapter permits.
    MalformedRequest(StateLimitError),
    /// One record cannot fit in the requested scan byte budget without splitting.
    ScanBoundTooSmall {
        /// Zero-based query position.
        query_index: usize,
        /// Estimated bytes required for the next complete record.
        required_bytes: usize,
    },
}

impl StateSnapshot {
    /// Creates a snapshot, rejecting reordered, mistyped, or invalid results.
    pub fn new(
        revision: StateRevision,
        batch: &ReadBatch,
        results: impl Into<Vec<ReadResult>>,
    ) -> Result<Self, InvalidStateSnapshot> {
        let results = results.into();
        if results.len() != batch.queries.len() {
            return Err(InvalidStateSnapshot::ResultCount {
                expected: batch.queries.len(),
                actual: results.len(),
            });
        }
        for (index, (query, result)) in batch.queries.iter().zip(&results).enumerate() {
            match (query, result) {
                (ReadQuery::Scan(scan), ReadResult::Scan(page)) => {
                    page.validate_against(batch.filesystem_id, scan, revision)?;
                }
                (
                    ReadQuery::DirectoryPage {
                        parent_inode_id,
                        after,
                        bounds,
                    },
                    ReadResult::DirectoryPage(page),
                ) => page.validate_against(*parent_inode_id, *after, *bounds, revision)?,
                (ReadQuery::OpenPinCount(expected), ReadResult::OpenPinCount { inode_id, .. })
                    if expected == inode_id => {}
                (
                    ReadQuery::InodeByQidPath(expected),
                    ReadResult::InodeByQidPath { qid_path, inode },
                ) if expected == qid_path => {
                    if let Some(inode) = inode {
                        if inode.qid_path() != *qid_path {
                            return Err(InvalidStateSnapshot::UnexpectedResult { index });
                        }
                        if inode.revision().get() > revision.get() {
                            return Err(InvalidStateSnapshot::RecordRevisionAfterSnapshot);
                        }
                    }
                }
                (
                    ReadQuery::InodeWithContentMetadata(expected),
                    ReadResult::InodeWithContentMetadata {
                        inode_id,
                        inode,
                        metadata,
                    },
                ) if expected == inode_id => match (inode, metadata) {
                    (None, None) => {}
                    (Some(inode), Some(metadata))
                        if inode.inode_id() == *inode_id
                            && inode.content_file_id() == Some(metadata.content_file_id())
                            && inode.content_context_id() == Some(metadata.context_id())
                            && inode.revision().get() <= revision.get()
                            && metadata.revision().get() <= revision.get() => {}
                    _ => return Err(InvalidStateSnapshot::UnexpectedResult { index }),
                },
                (query, ReadResult::Point { key, record })
                    if query.point_key(batch.filesystem_id).as_ref() == Some(key) =>
                {
                    if let Some(record) = record {
                        record
                            .validate_key(key)
                            .map_err(InvalidStateSnapshot::InvalidRecord)?;
                        if record.revision().get() > revision.get() {
                            return Err(InvalidStateSnapshot::RecordRevisionAfterSnapshot);
                        }
                    }
                }
                _ => return Err(InvalidStateSnapshot::UnexpectedResult { index }),
            }
        }
        Ok(Self {
            revision,
            results: results.into_boxed_slice(),
        })
    }

    /// Returns the single authoritative revision observed by all results.
    pub const fn revision(&self) -> StateRevision {
        self.revision
    }

    /// Returns results in the same order as the batch queries.
    pub fn results(&self) -> &[ReadResult] {
        &self.results
    }
}

/// Invalid adapter-produced read result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InvalidStateSnapshot {
    /// Result count did not match query count.
    ResultCount {
        /// Query count.
        expected: usize,
        /// Result count.
        actual: usize,
    },
    /// A result did not correspond to its positional query.
    UnexpectedResult {
        /// Zero-based result position.
        index: usize,
    },
    /// A scan returned a key or cursor of another family.
    UnexpectedRecordFamily {
        /// Requested family.
        expected: RecordFamily,
        /// Returned family.
        actual: RecordFamily,
    },
    /// A returned record did not match its key.
    InvalidRecord(RecordValidationError),
    /// Scan returned a record belonging to another filesystem.
    ScanOutsideFilesystem,
    /// Scan returned a record outside the query's parent or resume range.
    RecordOutsideScan,
    /// Scan records were not in strict canonical order.
    UnorderedScan,
    /// Scan returned more records than its item limit.
    ScanItemLimit,
    /// Scan result exceeded or overflowed its byte limit.
    ScanByteLimit,
    /// Resume cursor did not identify the final returned record.
    InvalidResumeCursor,
    /// A returned record revision is newer than the enclosing snapshot.
    RecordRevisionAfterSnapshot,
}

impl fmt::Display for InvalidStateSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ResultCount { expected, actual } => {
                write!(
                    formatter,
                    "read batch expected {expected} results but received {actual}"
                )
            }
            Self::UnexpectedResult { index } => {
                write!(formatter, "read result {index} does not match its query")
            }
            Self::UnexpectedRecordFamily { expected, actual } => write!(
                formatter,
                "scan expected {expected:?} records but received {actual:?}"
            ),
            Self::InvalidRecord(error) => error.fmt(formatter),
            Self::ScanOutsideFilesystem => {
                formatter.write_str("scan returned a record from another filesystem")
            }
            Self::RecordOutsideScan => {
                formatter.write_str("scan returned a record outside its requested range")
            }
            Self::UnorderedScan => formatter.write_str("scan records are not strictly ordered"),
            Self::ScanItemLimit => formatter.write_str("scan result exceeds its item bound"),
            Self::ScanByteLimit => formatter.write_str("scan result exceeds its byte bound"),
            Self::InvalidResumeCursor => {
                formatter.write_str("scan resume cursor does not match the final record")
            }
            Self::RecordRevisionAfterSnapshot => {
                formatter.write_str("record revision is newer than snapshot revision")
            }
        }
    }
}

impl std::error::Error for InvalidStateSnapshot {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_bounds_are_nonzero_and_contract_bounded() {
        let limits = StateLimits::default();
        assert!(ScanBounds::new(1, 1, limits).is_ok());
        assert_eq!(
            ScanBounds::new(0, 1, limits),
            Err(InvalidScanBounds::ZeroItems)
        );
        assert!(matches!(
            ScanBounds::new(limits.max_scan_items() + 1, 1, limits),
            Err(InvalidScanBounds::Limit(_))
        ));
    }

    #[test]
    fn point_queries_map_only_to_their_exact_semantic_key() {
        let filesystem_id = crate::FilesystemId::from_u128(1);
        let inode_id = InodeId::from_u128(2);
        assert_eq!(
            ReadQuery::Inode(inode_id).point_key(filesystem_id),
            Some(RecordKey::Inode(filesystem_id, inode_id))
        );
        let bounds = ScanBounds::new(1, 1, StateLimits::default()).unwrap();
        assert_eq!(
            ReadQuery::Scan(RecordScan::Inodes {
                after: None,
                bounds,
            })
            .point_key(filesystem_id),
            None
        );
        assert_eq!(
            ReadQuery::InodeByQidPath(QidPath::new(1).unwrap()).point_key(filesystem_id),
            None
        );
    }

    #[test]
    fn qid_lookup_results_are_positionally_bound() {
        let limits = StateLimits::default();
        let filesystem_id = FilesystemId::from_u128(1);
        let qid_path = QidPath::new(2).unwrap();
        let batch = ReadBatch::new(
            filesystem_id,
            ReadConsistency::LatestLinearizable,
            vec![ReadQuery::InodeByQidPath(qid_path)],
            limits,
        )
        .unwrap();
        assert!(
            StateSnapshot::new(
                StateRevision::new(1).unwrap(),
                &batch,
                vec![ReadResult::InodeByQidPath {
                    qid_path,
                    inode: None,
                }],
            )
            .is_ok()
        );
        assert!(matches!(
            StateSnapshot::new(
                StateRevision::new(1).unwrap(),
                &batch,
                vec![ReadResult::InodeByQidPath {
                    qid_path: QidPath::new(3).unwrap(),
                    inode: None,
                }],
            ),
            Err(InvalidStateSnapshot::UnexpectedResult { index: 0 })
        ));
    }

    #[test]
    fn snapshots_enforce_positional_query_association() {
        let limits = StateLimits::default();
        let filesystem_id = FilesystemId::from_u128(1);
        let inode_id = InodeId::from_u128(2);
        let batch = ReadBatch::new(
            filesystem_id,
            ReadConsistency::LatestLinearizable,
            vec![ReadQuery::Inode(inode_id)],
            limits,
        )
        .unwrap();
        let wrong = ReadResult::Point {
            key: RecordKey::Orphan(filesystem_id, inode_id),
            record: None,
        };
        assert!(matches!(
            StateSnapshot::new(StateRevision::new(1).unwrap(), &batch, vec![wrong]),
            Err(InvalidStateSnapshot::UnexpectedResult { index: 0 })
        ));
    }

    #[test]
    fn snapshots_validate_the_complete_scan_query_and_resume() {
        let limits = StateLimits::default();
        let filesystem_id = FilesystemId::from_u128(1);
        let other_filesystem = FilesystemId::from_u128(2);
        let bounds = ScanBounds::new(2, 1024, limits).unwrap();
        let batch = ReadBatch::new(
            filesystem_id,
            ReadConsistency::LatestLinearizable,
            vec![ReadQuery::Scan(RecordScan::Opens {
                after: None,
                bounds,
            })],
            limits,
        )
        .unwrap();
        let open_id = OpenId::from_u128(3);
        let record = StateRecord::Open(crate::OpenRecord::new(
            open_id,
            InodeId::from_u128(4),
            crate::ClientIncarnationId::from_u128(5),
            crate::OpenAccess::ReadOnly,
            false,
            crate::InodeGeneration::new(1).unwrap(),
            crate::RecordRevision::new(1).unwrap(),
        ));
        let page = ScanPage::new(
            RecordFamily::Open,
            vec![(RecordKey::Open(other_filesystem, open_id), record)],
            None,
        )
        .unwrap();
        assert_eq!(
            StateSnapshot::new(
                StateRevision::new(1).unwrap(),
                &batch,
                vec![ReadResult::Scan(page)],
            ),
            Err(InvalidStateSnapshot::ScanOutsideFilesystem)
        );

        let inode_batch = ReadBatch::new(
            filesystem_id,
            ReadConsistency::LatestLinearizable,
            vec![ReadQuery::Scan(RecordScan::Inodes {
                after: None,
                bounds,
            })],
            limits,
        )
        .unwrap();
        let empty_with_resume = ScanPage::new(
            RecordFamily::Inode,
            vec![],
            Some(ScanResume::Inode(InodeId::from_u128(9))),
        )
        .unwrap();
        assert_eq!(
            StateSnapshot::new(
                StateRevision::new(1).unwrap(),
                &inode_batch,
                vec![ReadResult::Scan(empty_with_resume)],
            ),
            Err(InvalidStateSnapshot::InvalidResumeCursor)
        );
    }
}
