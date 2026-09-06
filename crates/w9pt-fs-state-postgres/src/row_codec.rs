//! Lossless conversion between portable state records and PostgreSQL row values.

use core::fmt;

use w9pt_fs_state::{
    BoundedValueError, ClientIncarnationId, ContentMetadataRecord, DeviceNumbers, DirectoryCookie,
    DirectoryEntryRecord, DirectoryGeneration, EntryName, FencingToken, FilesystemId,
    FilesystemRecord, GroupId, InodeData, InodeGeneration, InodeId, InodeKind, InodeRecord,
    InodeTimes, InvalidValue, LeaseDeadline, LeaseId, LockGeneration, LockId, LockKind, LockOwner,
    LockRange, LockRangeEnd, LockRecord, MutationRecord, MutationResult, MutationResultKind,
    MutationRetention, OpenAccess, OpenId, OpenPinRecord, OpenRecord, OrphanRecord, PrincipalId,
    QidPath, RecordKey, RecordRevision, RecordValidationError, RequestFingerprint,
    ResultFormatVersion, StateLimitError, StateLimits, StateRecord, StateRevision, SymlinkTarget,
    UnixTimestamp, WriterIncarnationId, WriterLeaseRecord, WriterScopeId, XattrName, XattrRecord,
    XattrStagingId, XattrStagingRecord, XattrValue,
};
use w9pt_fs_storage::{
    ContentRef, Digest, FileId, InvalidContentRef, InvalidObjectKey, MutationId, ObjectKey,
    StorageMethod,
};

use crate::numeric::{NumericCodecError, decode_u64, encode_u64};

const REGULAR_FILE_TAG: i16 = 1;
const DIRECTORY_TAG: i16 = 2;
const SYMLINK_TAG: i16 = 3;
const CHARACTER_DEVICE_TAG: i16 = 4;
const BLOCK_DEVICE_TAG: i16 = 5;
const FIFO_TAG: i16 = 6;
const SOCKET_TAG: i16 = 7;

const OPEN_READ_ONLY_TAG: i16 = 1;
const OPEN_WRITE_ONLY_TAG: i16 = 2;
const OPEN_READ_WRITE_TAG: i16 = 3;
const OPEN_DIRECTORY_READ_TAG: i16 = 4;

const LOCK_SHARED_TAG: i16 = 1;
const LOCK_EXCLUSIVE_TAG: i16 = 2;

const STORAGE_RAW_TAG: i16 = 1;
const STORAGE_BLOCK_SPLIT_TAG: i16 = 2;

/// Primitive values corresponding to one row in a public record table.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SqlStateRecord {
    Filesystem(FilesystemRow),
    Inode(Box<InodeRow>),
    ContentMetadata(ContentMetadataRow),
    DirectoryEntry(DirectoryEntryRow),
    Open(OpenRow),
    OpenPin(OpenPinRow),
    Orphan(OrphanRow),
    Lock(LockRow),
    Xattr(XattrRow),
    XattrStaging(XattrStagingRow),
    Mutation(MutationRow),
    WriterLease(WriterLeaseRow),
}

impl SqlStateRecord {
    /// Encodes one exact semantic key/value pair without narrowing integers.
    pub(crate) fn encode(key: &RecordKey, record: &StateRecord) -> Result<Self, RowCodecError> {
        record
            .validate_key(key)
            .map_err(RowCodecError::InvalidRecord)?;
        let filesystem_id = key.filesystem_id().as_bytes().to_vec();
        Ok(match record {
            StateRecord::Filesystem(record) => Self::Filesystem(FilesystemRow {
                filesystem_id,
                state_revision: encode_u64(record.revision().get()),
                record_revision: encode_u64(record.record_revision().get()),
                root_inode_id: record.root_inode_id().as_bytes().to_vec(),
                next_qid_path: encode_u64(record.next_qid_path().get()),
                next_directory_cookie: encode_u64(record.next_directory_cookie().get()),
                policy_generation: encode_u64(record.policy_generation()),
            }),
            StateRecord::Inode(record) => {
                Self::Inode(Box::new(encode_inode(filesystem_id, record)))
            }
            StateRecord::ContentMetadata(record) => Self::ContentMetadata(ContentMetadataRow {
                filesystem_id,
                content_file_id: record.content_file_id().as_bytes().to_vec(),
                owner_inode_id: record.owner_inode_id().as_bytes().to_vec(),
                context_id: record.context_id().as_bytes().to_vec(),
                policy_format: i32::from(record.policy_format()),
                policy_bytes: record.policy_bytes().to_vec(),
                key_commitment: record.key_commitment().map(|bytes| bytes.to_vec()),
                wrapped_key_bytes: record.wrapped_key_bytes().map(<[u8]>::to_vec),
                record_revision: encode_u64(record.revision().get()),
            }),
            StateRecord::DirectoryEntry(record) => Self::DirectoryEntry(DirectoryEntryRow {
                filesystem_id,
                parent_inode_id: record.parent_inode_id().as_bytes().to_vec(),
                name: record.name().as_bytes().to_vec(),
                cookie: encode_u64(record.cookie().get()),
                child_inode_id: record.child_inode_id().as_bytes().to_vec(),
                record_revision: encode_u64(record.revision().get()),
            }),
            StateRecord::Open(record) => Self::Open(OpenRow {
                filesystem_id,
                open_id: record.open_id().as_bytes().to_vec(),
                inode_id: record.inode_id().as_bytes().to_vec(),
                client_incarnation_id: record.client_incarnation().as_bytes().to_vec(),
                access: encode_open_access(record.access()),
                append: record.append(),
                retained_inode_generation: encode_u64(record.retained_inode_generation().get()),
                record_revision: encode_u64(record.revision().get()),
            }),
            StateRecord::OpenPin(record) => Self::OpenPin(OpenPinRow {
                filesystem_id,
                inode_id: record.inode_id().as_bytes().to_vec(),
                open_id: record.open_id().as_bytes().to_vec(),
                record_revision: encode_u64(record.revision().get()),
            }),
            StateRecord::Orphan(record) => Self::Orphan(OrphanRow {
                filesystem_id,
                inode_id: record.inode_id().as_bytes().to_vec(),
                open_pin_count: encode_u64(record.open_pin_count()),
                orphaned_revision: encode_u64(record.orphaned_revision().get()),
                record_revision: encode_u64(record.revision().get()),
            }),
            StateRecord::Lock(record) => Self::Lock(LockRow {
                filesystem_id,
                inode_id: record.inode_id().as_bytes().to_vec(),
                lock_id: record.lock_id().as_bytes().to_vec(),
                range_start: encode_u64(record.range().start()),
                range_end: match record.range().end() {
                    LockRangeEnd::Exclusive(end) => Some(encode_u64(end)),
                    LockRangeEnd::ThroughEof => None,
                },
                kind: encode_lock_kind(record.kind()),
                owner_client_incarnation_id: record
                    .owner()
                    .client_incarnation()
                    .as_bytes()
                    .to_vec(),
                owner_open_id: record.owner().open_id().as_bytes().to_vec(),
                lock_generation: encode_u64(record.generation().get()),
                record_revision: encode_u64(record.revision().get()),
            }),
            StateRecord::Xattr(record) => Self::Xattr(XattrRow {
                filesystem_id,
                inode_id: record.inode_id().as_bytes().to_vec(),
                name: record.name().as_bytes().to_vec(),
                value: record.value().as_bytes().to_vec(),
                record_revision: encode_u64(record.revision().get()),
            }),
            StateRecord::XattrStaging(record) => Self::XattrStaging(XattrStagingRow {
                filesystem_id,
                staging_id: record.staging_id().as_bytes().to_vec(),
                inode_id: record.inode_id().as_bytes().to_vec(),
                name: record.name().as_bytes().to_vec(),
                expected_size: encode_u64(record.expected_size()),
                staged_bytes: record.bytes().as_bytes().to_vec(),
                record_revision: encode_u64(record.revision().get()),
            }),
            StateRecord::Mutation(record) => Self::Mutation(MutationRow {
                filesystem_id,
                mutation_id: record.mutation_id().as_bytes().to_vec(),
                request_fingerprint: record.fingerprint().as_bytes().to_vec(),
                client_incarnation_id: record.client_incarnation().as_bytes().to_vec(),
                writer_scope_id: record.writer_scope().as_bytes().to_vec(),
                writer_incarnation_id: record.writer_incarnation().as_bytes().to_vec(),
                fencing_token: encode_u64(record.fencing_token().get()),
                result_kind: i32::from(record.result().kind().get()),
                result_format: i32::from(record.result().format().get()),
                result_bytes: record.result().bytes().to_vec(),
                committed_revision: encode_u64(record.committed_revision().get()),
                retention_horizon: encode_u64(record.retention().horizon()),
                record_revision: encode_u64(record.revision().get()),
            }),
            StateRecord::WriterLease(record) => Self::WriterLease(WriterLeaseRow {
                filesystem_id,
                writer_scope_id: record.scope().as_bytes().to_vec(),
                greatest_fencing_token: encode_u64(record.fencing_token().get()),
                active_holder_id: record.holder().as_bytes().to_vec(),
                active_lease_id: record.lease_id().as_bytes().to_vec(),
                active_deadline_tick: encode_u64(record.deadline().ticks()),
                active_record_revision: encode_u64(record.revision().get()),
            }),
        })
    }

    /// Decodes and independently validates one persisted public record row.
    pub(crate) fn decode(
        self,
        limits: StateLimits,
    ) -> Result<(RecordKey, StateRecord), RowCodecError> {
        let (key, record) = match self {
            Self::Filesystem(row) => decode_filesystem(row)?,
            Self::Inode(row) => decode_inode(*row, limits)?,
            Self::ContentMetadata(row) => decode_content_metadata(row, limits)?,
            Self::DirectoryEntry(row) => decode_directory_entry(row, limits)?,
            Self::Open(row) => decode_open(row)?,
            Self::OpenPin(row) => decode_open_pin(row)?,
            Self::Orphan(row) => decode_orphan(row)?,
            Self::Lock(row) => decode_lock(row)?,
            Self::Xattr(row) => decode_xattr(row, limits)?,
            Self::XattrStaging(row) => decode_xattr_staging(row, limits)?,
            Self::Mutation(row) => decode_mutation(row, limits)?,
            Self::WriterLease(row) => decode_writer_lease(row)?,
        };
        record
            .validate_key(&key)
            .map_err(RowCodecError::InvalidRecord)?;
        record
            .validate_against_limits(limits)
            .map_err(RowCodecError::Limit)?;
        Ok((key, record))
    }
}

/// Primitive `filesystem_records` row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FilesystemRow {
    pub(crate) filesystem_id: Vec<u8>,
    pub(crate) state_revision: String,
    pub(crate) record_revision: String,
    pub(crate) root_inode_id: Vec<u8>,
    pub(crate) next_qid_path: String,
    pub(crate) next_directory_cookie: String,
    pub(crate) policy_generation: String,
}

/// Primitive `inodes` row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InodeRow {
    pub(crate) filesystem_id: Vec<u8>,
    pub(crate) inode_id: Vec<u8>,
    pub(crate) qid_path: String,
    pub(crate) record_revision: String,
    pub(crate) mode: i32,
    pub(crate) owner: Vec<u8>,
    pub(crate) group_id: Vec<u8>,
    pub(crate) accessed_seconds: i64,
    pub(crate) accessed_nanoseconds: i32,
    pub(crate) modified_seconds: i64,
    pub(crate) modified_nanoseconds: i32,
    pub(crate) changed_seconds: i64,
    pub(crate) changed_nanoseconds: i32,
    pub(crate) created_seconds: i64,
    pub(crate) created_nanoseconds: i32,
    pub(crate) logical_size: String,
    pub(crate) link_count: String,
    pub(crate) inode_generation: String,
    pub(crate) kind: i16,
    pub(crate) content_file_id: Option<Vec<u8>>,
    pub(crate) content_context_id: Option<Vec<u8>>,
    pub(crate) data_generation: Option<String>,
    pub(crate) content_generation: Option<String>,
    pub(crate) content_logical_size: Option<String>,
    pub(crate) content_manifest_key: Option<String>,
    pub(crate) content_manifest_digest: Option<Vec<u8>>,
    pub(crate) content_storage_method: Option<i16>,
    pub(crate) directory_generation: Option<String>,
    pub(crate) directory_parent_inode_id: Option<Vec<u8>>,
    pub(crate) symlink_target: Option<Vec<u8>>,
    pub(crate) device_major: Option<i64>,
    pub(crate) device_minor: Option<i64>,
}

/// Primitive `content_metadata` row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ContentMetadataRow {
    pub(crate) filesystem_id: Vec<u8>,
    pub(crate) content_file_id: Vec<u8>,
    pub(crate) owner_inode_id: Vec<u8>,
    pub(crate) context_id: Vec<u8>,
    pub(crate) policy_format: i32,
    pub(crate) policy_bytes: Vec<u8>,
    pub(crate) key_commitment: Option<Vec<u8>>,
    pub(crate) wrapped_key_bytes: Option<Vec<u8>>,
    pub(crate) record_revision: String,
}

/// Primitive `directory_entries` row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DirectoryEntryRow {
    pub(crate) filesystem_id: Vec<u8>,
    pub(crate) parent_inode_id: Vec<u8>,
    pub(crate) name: Vec<u8>,
    pub(crate) cookie: String,
    pub(crate) child_inode_id: Vec<u8>,
    pub(crate) record_revision: String,
}

/// Primitive `opens` row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OpenRow {
    pub(crate) filesystem_id: Vec<u8>,
    pub(crate) open_id: Vec<u8>,
    pub(crate) inode_id: Vec<u8>,
    pub(crate) client_incarnation_id: Vec<u8>,
    pub(crate) access: i16,
    pub(crate) append: bool,
    pub(crate) retained_inode_generation: String,
    pub(crate) record_revision: String,
}

/// Primitive `open_pins` row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OpenPinRow {
    pub(crate) filesystem_id: Vec<u8>,
    pub(crate) inode_id: Vec<u8>,
    pub(crate) open_id: Vec<u8>,
    pub(crate) record_revision: String,
}

/// Primitive `orphans` row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OrphanRow {
    pub(crate) filesystem_id: Vec<u8>,
    pub(crate) inode_id: Vec<u8>,
    pub(crate) open_pin_count: String,
    pub(crate) orphaned_revision: String,
    pub(crate) record_revision: String,
}

/// Primitive `locks` row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LockRow {
    pub(crate) filesystem_id: Vec<u8>,
    pub(crate) inode_id: Vec<u8>,
    pub(crate) lock_id: Vec<u8>,
    pub(crate) range_start: String,
    pub(crate) range_end: Option<String>,
    pub(crate) kind: i16,
    pub(crate) owner_client_incarnation_id: Vec<u8>,
    pub(crate) owner_open_id: Vec<u8>,
    pub(crate) lock_generation: String,
    pub(crate) record_revision: String,
}

/// Primitive `xattrs` row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct XattrRow {
    pub(crate) filesystem_id: Vec<u8>,
    pub(crate) inode_id: Vec<u8>,
    pub(crate) name: Vec<u8>,
    pub(crate) value: Vec<u8>,
    pub(crate) record_revision: String,
}

/// Primitive `xattr_staging` row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct XattrStagingRow {
    pub(crate) filesystem_id: Vec<u8>,
    pub(crate) staging_id: Vec<u8>,
    pub(crate) inode_id: Vec<u8>,
    pub(crate) name: Vec<u8>,
    pub(crate) expected_size: String,
    pub(crate) staged_bytes: Vec<u8>,
    pub(crate) record_revision: String,
}

/// Primitive `mutation_results` row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MutationRow {
    pub(crate) filesystem_id: Vec<u8>,
    pub(crate) mutation_id: Vec<u8>,
    pub(crate) request_fingerprint: Vec<u8>,
    pub(crate) client_incarnation_id: Vec<u8>,
    pub(crate) writer_scope_id: Vec<u8>,
    pub(crate) writer_incarnation_id: Vec<u8>,
    pub(crate) fencing_token: String,
    pub(crate) result_kind: i32,
    pub(crate) result_format: i32,
    pub(crate) result_bytes: Vec<u8>,
    pub(crate) committed_revision: String,
    pub(crate) retention_horizon: String,
    pub(crate) record_revision: String,
}

/// Primitive active projection of one `writer_fences` row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WriterLeaseRow {
    pub(crate) filesystem_id: Vec<u8>,
    pub(crate) writer_scope_id: Vec<u8>,
    pub(crate) greatest_fencing_token: String,
    pub(crate) active_holder_id: Vec<u8>,
    pub(crate) active_lease_id: Vec<u8>,
    pub(crate) active_deadline_tick: String,
    pub(crate) active_record_revision: String,
}

fn encode_inode(filesystem_id: Vec<u8>, record: &InodeRecord) -> InodeRow {
    let times = record.times();
    let mut row = InodeRow {
        filesystem_id,
        inode_id: record.inode_id().as_bytes().to_vec(),
        qid_path: encode_u64(record.qid_path().get()),
        record_revision: encode_u64(record.revision().get()),
        mode: i32::try_from(record.mode()).expect("validated inode mode fits i32"),
        owner: record.owner().as_bytes().to_vec(),
        group_id: record.group().as_bytes().to_vec(),
        accessed_seconds: times.accessed.seconds(),
        accessed_nanoseconds: i32::try_from(times.accessed.nanoseconds())
            .expect("validated nanoseconds fit i32"),
        modified_seconds: times.modified.seconds(),
        modified_nanoseconds: i32::try_from(times.modified.nanoseconds())
            .expect("validated nanoseconds fit i32"),
        changed_seconds: times.changed.seconds(),
        changed_nanoseconds: i32::try_from(times.changed.nanoseconds())
            .expect("validated nanoseconds fit i32"),
        created_seconds: times.created.seconds(),
        created_nanoseconds: i32::try_from(times.created.nanoseconds())
            .expect("validated nanoseconds fit i32"),
        logical_size: encode_u64(record.logical_size()),
        link_count: encode_u64(record.link_count()),
        inode_generation: encode_u64(record.inode_generation().get()),
        kind: 0,
        content_file_id: None,
        content_context_id: record.content_context_id().map(|id| id.as_bytes().to_vec()),
        data_generation: None,
        content_generation: None,
        content_logical_size: None,
        content_manifest_key: None,
        content_manifest_digest: None,
        content_storage_method: None,
        directory_generation: None,
        directory_parent_inode_id: None,
        symlink_target: None,
        device_major: None,
        device_minor: None,
    };
    match record.data() {
        InodeData::RegularFile {
            content_file_id,
            content,
            data_generation,
        } => {
            row.kind = REGULAR_FILE_TAG;
            row.content_file_id = Some(content_file_id.as_bytes().to_vec());
            row.data_generation = Some(encode_u64(*data_generation));
            if let Some(content) = content {
                row.content_generation = Some(encode_u64(content.generation()));
                row.content_logical_size = Some(encode_u64(content.logical_size()));
                row.content_manifest_key = Some(content.manifest_key().as_str().to_owned());
                row.content_manifest_digest = Some(content.manifest_digest().as_bytes().to_vec());
                row.content_storage_method = Some(encode_storage_method(content.method()));
            }
        }
        InodeData::Directory {
            generation,
            parent_inode_id,
        } => {
            row.kind = DIRECTORY_TAG;
            row.directory_generation = Some(encode_u64(generation.get()));
            row.directory_parent_inode_id = Some(parent_inode_id.as_bytes().to_vec());
        }
        InodeData::Symlink { target } => {
            row.kind = SYMLINK_TAG;
            row.symlink_target = Some(target.as_bytes().to_vec());
        }
        InodeData::CharacterDevice(device) => {
            row.kind = CHARACTER_DEVICE_TAG;
            row.device_major = Some(i64::from(device.major));
            row.device_minor = Some(i64::from(device.minor));
        }
        InodeData::BlockDevice(device) => {
            row.kind = BLOCK_DEVICE_TAG;
            row.device_major = Some(i64::from(device.major));
            row.device_minor = Some(i64::from(device.minor));
        }
        InodeData::Fifo => row.kind = FIFO_TAG,
        InodeData::Socket => row.kind = SOCKET_TAG,
    }
    row
}

fn decode_filesystem(row: FilesystemRow) -> Result<(RecordKey, StateRecord), RowCodecError> {
    let filesystem_id = FilesystemId::new(fixed_bytes("filesystem_id", row.filesystem_id)?);
    let record = FilesystemRecord::new(
        filesystem_id,
        decode_state_revision("state_revision", &row.state_revision)?,
        decode_record_revision("record_revision", &row.record_revision)?,
        InodeId::new(fixed_bytes("root_inode_id", row.root_inode_id)?),
        decode_qid_path("next_qid_path", &row.next_qid_path)?,
        DirectoryCookie::new(decode_u64_value(
            "next_directory_cookie",
            &row.next_directory_cookie,
        )?),
        decode_u64_value("policy_generation", &row.policy_generation)?,
    )
    .map_err(RowCodecError::InvalidRecord)?;
    Ok((
        RecordKey::Filesystem(filesystem_id),
        StateRecord::Filesystem(record),
    ))
}

fn decode_inode(
    row: InodeRow,
    limits: StateLimits,
) -> Result<(RecordKey, StateRecord), RowCodecError> {
    let filesystem_id = FilesystemId::new(fixed_bytes("filesystem_id", row.filesystem_id.clone())?);
    let inode_id = InodeId::new(fixed_bytes("inode_id", row.inode_id.clone())?);
    let data = decode_inode_data(&row, limits)?;
    let qid_path = decode_qid_path("qid_path", &row.qid_path)?;
    let revision = decode_record_revision("record_revision", &row.record_revision)?;
    let mode = decode_u32("mode", i64::from(row.mode))?;
    let owner = PrincipalId::new(row.owner, limits).map_err(RowCodecError::Bounded)?;
    let group = GroupId::new(row.group_id, limits).map_err(RowCodecError::Bounded)?;
    let times = InodeTimes {
        accessed: decode_timestamp(
            "accessed_nanoseconds",
            row.accessed_seconds,
            row.accessed_nanoseconds,
        )?,
        modified: decode_timestamp(
            "modified_nanoseconds",
            row.modified_seconds,
            row.modified_nanoseconds,
        )?,
        changed: decode_timestamp(
            "changed_nanoseconds",
            row.changed_seconds,
            row.changed_nanoseconds,
        )?,
        created: decode_timestamp(
            "created_nanoseconds",
            row.created_seconds,
            row.created_nanoseconds,
        )?,
    };
    let logical_size = decode_u64_value("logical_size", &row.logical_size)?;
    let link_count = decode_u64_value("link_count", &row.link_count)?;
    let inode_generation = decode_inode_generation("inode_generation", &row.inode_generation)?;
    let record = match (data.kind(), row.content_context_id) {
        (InodeKind::RegularFile, Some(context_id)) => InodeRecord::new_regular(
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
            w9pt_fs_storage::ContentContextId::new(fixed_bytes("content_context_id", context_id)?),
            data,
        )
        .map_err(RowCodecError::InvalidRecord)?,
        (InodeKind::RegularFile, None) => {
            return Err(RowCodecError::MissingField {
                record: "regular inode",
                field: "content_context_id",
            });
        }
        (_, Some(_)) => {
            return Err(RowCodecError::InvalidShape {
                record: "inode",
                detail: "content context belongs only to regular files",
            });
        }
        (_, None) => InodeRecord::new(
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
            data,
        )
        .map_err(RowCodecError::InvalidRecord)?,
    };
    Ok((
        RecordKey::Inode(filesystem_id, inode_id),
        StateRecord::Inode(record),
    ))
}

fn decode_content_metadata(
    row: ContentMetadataRow,
    limits: StateLimits,
) -> Result<(RecordKey, StateRecord), RowCodecError> {
    let filesystem_id = FilesystemId::new(fixed_bytes("filesystem_id", row.filesystem_id)?);
    let file_id = FileId::new(fixed_bytes("content_file_id", row.content_file_id)?);
    let record = ContentMetadataRecord::new(
        InodeId::new(fixed_bytes("owner_inode_id", row.owner_inode_id)?),
        file_id,
        w9pt_fs_storage::ContentContextId::new(fixed_bytes("context_id", row.context_id)?),
        decode_u16("policy_format", i64::from(row.policy_format))?,
        row.policy_bytes,
        row.key_commitment
            .map(|bytes| fixed_bytes("key_commitment", bytes))
            .transpose()?,
        row.wrapped_key_bytes,
        decode_record_revision("record_revision", &row.record_revision)?,
        limits,
    )
    .map_err(|_| RowCodecError::InvalidShape {
        record: "content metadata",
        detail: "opaque content metadata failed validation",
    })?;
    Ok((
        RecordKey::ContentMetadata(filesystem_id, file_id),
        StateRecord::ContentMetadata(record),
    ))
}

fn decode_inode_data(row: &InodeRow, limits: StateLimits) -> Result<InodeData, RowCodecError> {
    match row.kind {
        REGULAR_FILE_TAG => {
            require_inode_shape(
                "regular file",
                row.directory_generation.is_none()
                    && row.directory_parent_inode_id.is_none()
                    && row.symlink_target.is_none()
                    && row.device_major.is_none()
                    && row.device_minor.is_none(),
            )?;
            let content_file_id = FileId::new(fixed_bytes(
                "content_file_id",
                required("regular file", "content_file_id", &row.content_file_id)?.clone(),
            )?);
            let data_generation = decode_u64_value(
                "data_generation",
                required("regular file", "data_generation", &row.data_generation)?,
            )?;
            let content_fields = [
                row.content_generation.is_some(),
                row.content_logical_size.is_some(),
                row.content_manifest_key.is_some(),
                row.content_manifest_digest.is_some(),
                row.content_storage_method.is_some(),
            ];
            let content = if content_fields.iter().all(|present| !present) {
                None
            } else if content_fields.iter().all(|present| *present) {
                Some(
                    ContentRef::from_persisted(
                        content_file_id,
                        decode_u64_value(
                            "content_generation",
                            row.content_generation
                                .as_deref()
                                .expect("presence was checked"),
                        )?,
                        decode_u64_value(
                            "content_logical_size",
                            row.content_logical_size
                                .as_deref()
                                .expect("presence was checked"),
                        )?,
                        ObjectKey::new(
                            row.content_manifest_key
                                .as_ref()
                                .expect("presence was checked")
                                .clone(),
                        )
                        .map_err(RowCodecError::InvalidObjectKey)?,
                        Digest::new(fixed_bytes(
                            "content_manifest_digest",
                            row.content_manifest_digest
                                .as_ref()
                                .expect("presence was checked")
                                .clone(),
                        )?),
                        decode_storage_method(
                            *row.content_storage_method
                                .as_ref()
                                .expect("presence was checked"),
                        )?,
                    )
                    .map_err(RowCodecError::InvalidContentRef)?,
                )
            } else {
                return Err(RowCodecError::InvalidShape {
                    record: "inode",
                    detail: "content reference fields are only partially present",
                });
            };
            Ok(InodeData::RegularFile {
                content_file_id,
                content,
                data_generation,
            })
        }
        DIRECTORY_TAG => {
            require_inode_shape("directory", only_directory_generation(row))?;
            Ok(InodeData::Directory {
                generation: decode_directory_generation(
                    "directory_generation",
                    required(
                        "directory",
                        "directory_generation",
                        &row.directory_generation,
                    )?,
                )?,
                parent_inode_id: InodeId::new(fixed_bytes(
                    "directory_parent_inode_id",
                    required(
                        "directory",
                        "directory_parent_inode_id",
                        &row.directory_parent_inode_id,
                    )?
                    .clone(),
                )?),
            })
        }
        SYMLINK_TAG => {
            require_inode_shape("symlink", only_symlink_target(row))?;
            Ok(InodeData::Symlink {
                target: SymlinkTarget::new(
                    required("symlink", "symlink_target", &row.symlink_target)?.clone(),
                    limits,
                )
                .map_err(RowCodecError::Bounded)?,
            })
        }
        CHARACTER_DEVICE_TAG | BLOCK_DEVICE_TAG => {
            require_inode_shape("device", only_device_numbers(row))?;
            let device = DeviceNumbers {
                major: decode_u32(
                    "device_major",
                    *required("device", "device_major", &row.device_major)?,
                )?,
                minor: decode_u32(
                    "device_minor",
                    *required("device", "device_minor", &row.device_minor)?,
                )?,
            };
            if row.kind == CHARACTER_DEVICE_TAG {
                Ok(InodeData::CharacterDevice(device))
            } else {
                Ok(InodeData::BlockDevice(device))
            }
        }
        FIFO_TAG | SOCKET_TAG => {
            require_inode_shape("special inode", no_kind_specific_values(row))?;
            if row.kind == FIFO_TAG {
                Ok(InodeData::Fifo)
            } else {
                Ok(InodeData::Socket)
            }
        }
        tag => Err(RowCodecError::UnknownTag {
            field: "inode kind",
            tag,
        }),
    }
}

fn decode_directory_entry(
    row: DirectoryEntryRow,
    limits: StateLimits,
) -> Result<(RecordKey, StateRecord), RowCodecError> {
    let filesystem_id = FilesystemId::new(fixed_bytes("filesystem_id", row.filesystem_id)?);
    let parent_inode_id = InodeId::new(fixed_bytes("parent_inode_id", row.parent_inode_id)?);
    let name = EntryName::new(row.name, limits).map_err(RowCodecError::Bounded)?;
    let record = DirectoryEntryRecord::new(
        parent_inode_id,
        name.clone(),
        DirectoryCookie::new(decode_u64_value("cookie", &row.cookie)?),
        InodeId::new(fixed_bytes("child_inode_id", row.child_inode_id)?),
        decode_record_revision("record_revision", &row.record_revision)?,
    )
    .map_err(RowCodecError::InvalidRecord)?;
    Ok((
        RecordKey::DirectoryEntry(filesystem_id, parent_inode_id, name),
        StateRecord::DirectoryEntry(record),
    ))
}

fn decode_open(row: OpenRow) -> Result<(RecordKey, StateRecord), RowCodecError> {
    let filesystem_id = FilesystemId::new(fixed_bytes("filesystem_id", row.filesystem_id)?);
    let open_id = OpenId::new(fixed_bytes("open_id", row.open_id)?);
    let record = OpenRecord::new(
        open_id,
        InodeId::new(fixed_bytes("inode_id", row.inode_id)?),
        ClientIncarnationId::new(fixed_bytes(
            "client_incarnation_id",
            row.client_incarnation_id,
        )?),
        decode_open_access(row.access)?,
        row.append,
        decode_inode_generation("retained_inode_generation", &row.retained_inode_generation)?,
        decode_record_revision("record_revision", &row.record_revision)?,
    );
    Ok((
        RecordKey::Open(filesystem_id, open_id),
        StateRecord::Open(record),
    ))
}

fn decode_open_pin(row: OpenPinRow) -> Result<(RecordKey, StateRecord), RowCodecError> {
    let filesystem_id = FilesystemId::new(fixed_bytes("filesystem_id", row.filesystem_id)?);
    let inode_id = InodeId::new(fixed_bytes("inode_id", row.inode_id)?);
    let open_id = OpenId::new(fixed_bytes("open_id", row.open_id)?);
    Ok((
        RecordKey::OpenPin(filesystem_id, inode_id, open_id),
        StateRecord::OpenPin(OpenPinRecord::new(
            inode_id,
            open_id,
            decode_record_revision("record_revision", &row.record_revision)?,
        )),
    ))
}

fn decode_orphan(row: OrphanRow) -> Result<(RecordKey, StateRecord), RowCodecError> {
    let filesystem_id = FilesystemId::new(fixed_bytes("filesystem_id", row.filesystem_id)?);
    let inode_id = InodeId::new(fixed_bytes("inode_id", row.inode_id)?);
    let record = OrphanRecord::new(
        inode_id,
        decode_u64_value("open_pin_count", &row.open_pin_count)?,
        decode_state_revision("orphaned_revision", &row.orphaned_revision)?,
        decode_record_revision("record_revision", &row.record_revision)?,
    )
    .map_err(RowCodecError::InvalidRecord)?;
    Ok((
        RecordKey::Orphan(filesystem_id, inode_id),
        StateRecord::Orphan(record),
    ))
}

fn decode_lock(row: LockRow) -> Result<(RecordKey, StateRecord), RowCodecError> {
    let filesystem_id = FilesystemId::new(fixed_bytes("filesystem_id", row.filesystem_id)?);
    let inode_id = InodeId::new(fixed_bytes("inode_id", row.inode_id)?);
    let lock_id = LockId::new(fixed_bytes("lock_id", row.lock_id)?);
    let start = decode_u64_value("range_start", &row.range_start)?;
    let range = match row.range_end {
        Some(end) => LockRange::finite(start, decode_u64_value("range_end", &end)?)
            .map_err(RowCodecError::InvalidRecord)?,
        None => LockRange::through_eof(start),
    };
    let record = LockRecord::new(
        lock_id,
        inode_id,
        range,
        decode_lock_kind(row.kind)?,
        LockOwner::new(
            ClientIncarnationId::new(fixed_bytes(
                "owner_client_incarnation_id",
                row.owner_client_incarnation_id,
            )?),
            OpenId::new(fixed_bytes("owner_open_id", row.owner_open_id)?),
        ),
        decode_lock_generation("lock_generation", &row.lock_generation)?,
        decode_record_revision("record_revision", &row.record_revision)?,
    );
    Ok((
        RecordKey::Lock(filesystem_id, inode_id, lock_id),
        StateRecord::Lock(record),
    ))
}

fn decode_xattr(
    row: XattrRow,
    limits: StateLimits,
) -> Result<(RecordKey, StateRecord), RowCodecError> {
    let filesystem_id = FilesystemId::new(fixed_bytes("filesystem_id", row.filesystem_id)?);
    let inode_id = InodeId::new(fixed_bytes("inode_id", row.inode_id)?);
    let name = XattrName::new(row.name, limits).map_err(RowCodecError::Bounded)?;
    let record = XattrRecord::new(
        inode_id,
        name.clone(),
        XattrValue::new(row.value, limits).map_err(RowCodecError::Bounded)?,
        decode_record_revision("record_revision", &row.record_revision)?,
    );
    Ok((
        RecordKey::Xattr(filesystem_id, inode_id, name),
        StateRecord::Xattr(record),
    ))
}

fn decode_xattr_staging(
    row: XattrStagingRow,
    limits: StateLimits,
) -> Result<(RecordKey, StateRecord), RowCodecError> {
    let filesystem_id = FilesystemId::new(fixed_bytes("filesystem_id", row.filesystem_id)?);
    let staging_id = XattrStagingId::new(fixed_bytes("staging_id", row.staging_id)?);
    let record = XattrStagingRecord::new(
        staging_id,
        InodeId::new(fixed_bytes("inode_id", row.inode_id)?),
        XattrName::new(row.name, limits).map_err(RowCodecError::Bounded)?,
        decode_u64_value("expected_size", &row.expected_size)?,
        XattrValue::new(row.staged_bytes, limits).map_err(RowCodecError::Bounded)?,
        decode_record_revision("record_revision", &row.record_revision)?,
        limits,
    )
    .map_err(RowCodecError::InvalidRecord)?;
    Ok((
        RecordKey::XattrStaging(filesystem_id, staging_id),
        StateRecord::XattrStaging(record),
    ))
}

fn decode_mutation(
    row: MutationRow,
    limits: StateLimits,
) -> Result<(RecordKey, StateRecord), RowCodecError> {
    let filesystem_id = FilesystemId::new(fixed_bytes("filesystem_id", row.filesystem_id)?);
    let mutation_id = MutationId::new(fixed_bytes("mutation_id", row.mutation_id)?);
    let result_kind =
        MutationResultKind::new(decode_u16("result_kind", i64::from(row.result_kind))?)
            .map_err(RowCodecError::Bounded)?;
    let result_format =
        ResultFormatVersion::new(decode_u16("result_format", i64::from(row.result_format))?)
            .map_err(RowCodecError::Bounded)?;
    let record = MutationRecord::new(
        filesystem_id,
        mutation_id,
        RequestFingerprint::new(fixed_bytes("request_fingerprint", row.request_fingerprint)?),
        ClientIncarnationId::new(fixed_bytes(
            "client_incarnation_id",
            row.client_incarnation_id,
        )?),
        WriterScopeId::new(fixed_bytes("writer_scope_id", row.writer_scope_id)?),
        WriterIncarnationId::new(fixed_bytes(
            "writer_incarnation_id",
            row.writer_incarnation_id,
        )?),
        decode_fencing_token("fencing_token", &row.fencing_token)?,
        MutationResult::new(result_kind, result_format, row.result_bytes, limits)
            .map_err(RowCodecError::Bounded)?,
        decode_state_revision("committed_revision", &row.committed_revision)?,
        MutationRetention::new(decode_u64_value(
            "retention_horizon",
            &row.retention_horizon,
        )?),
        decode_record_revision("record_revision", &row.record_revision)?,
    );
    Ok((
        RecordKey::Mutation(filesystem_id, mutation_id),
        StateRecord::Mutation(record),
    ))
}

fn decode_writer_lease(row: WriterLeaseRow) -> Result<(RecordKey, StateRecord), RowCodecError> {
    let filesystem_id = FilesystemId::new(fixed_bytes("filesystem_id", row.filesystem_id)?);
    let scope = WriterScopeId::new(fixed_bytes("writer_scope_id", row.writer_scope_id)?);
    let record = WriterLeaseRecord::new(
        filesystem_id,
        scope,
        WriterIncarnationId::new(fixed_bytes("active_holder_id", row.active_holder_id)?),
        LeaseId::new(fixed_bytes("active_lease_id", row.active_lease_id)?),
        LeaseDeadline::new(decode_u64_value(
            "active_deadline_tick",
            &row.active_deadline_tick,
        )?),
        decode_fencing_token("greatest_fencing_token", &row.greatest_fencing_token)?,
        decode_record_revision("active_record_revision", &row.active_record_revision)?,
    );
    Ok((
        RecordKey::WriterLease(filesystem_id, scope),
        StateRecord::WriterLease(record),
    ))
}

fn encode_open_access(access: OpenAccess) -> i16 {
    match access {
        OpenAccess::ReadOnly => OPEN_READ_ONLY_TAG,
        OpenAccess::WriteOnly => OPEN_WRITE_ONLY_TAG,
        OpenAccess::ReadWrite => OPEN_READ_WRITE_TAG,
        OpenAccess::DirectoryRead => OPEN_DIRECTORY_READ_TAG,
    }
}

fn decode_open_access(tag: i16) -> Result<OpenAccess, RowCodecError> {
    match tag {
        OPEN_READ_ONLY_TAG => Ok(OpenAccess::ReadOnly),
        OPEN_WRITE_ONLY_TAG => Ok(OpenAccess::WriteOnly),
        OPEN_READ_WRITE_TAG => Ok(OpenAccess::ReadWrite),
        OPEN_DIRECTORY_READ_TAG => Ok(OpenAccess::DirectoryRead),
        tag => Err(RowCodecError::UnknownTag {
            field: "open access",
            tag,
        }),
    }
}

fn encode_lock_kind(kind: LockKind) -> i16 {
    match kind {
        LockKind::Shared => LOCK_SHARED_TAG,
        LockKind::Exclusive => LOCK_EXCLUSIVE_TAG,
    }
}

fn decode_lock_kind(tag: i16) -> Result<LockKind, RowCodecError> {
    match tag {
        LOCK_SHARED_TAG => Ok(LockKind::Shared),
        LOCK_EXCLUSIVE_TAG => Ok(LockKind::Exclusive),
        tag => Err(RowCodecError::UnknownTag {
            field: "lock kind",
            tag,
        }),
    }
}

fn encode_storage_method(method: StorageMethod) -> i16 {
    match method {
        StorageMethod::Raw => STORAGE_RAW_TAG,
        StorageMethod::BlockSplit => STORAGE_BLOCK_SPLIT_TAG,
    }
}

fn decode_storage_method(tag: i16) -> Result<StorageMethod, RowCodecError> {
    match tag {
        STORAGE_RAW_TAG => Ok(StorageMethod::Raw),
        STORAGE_BLOCK_SPLIT_TAG => Ok(StorageMethod::BlockSplit),
        tag => Err(RowCodecError::UnknownTag {
            field: "content storage method",
            tag,
        }),
    }
}

fn decode_timestamp(
    nanoseconds_field: &'static str,
    seconds: i64,
    nanoseconds: i32,
) -> Result<UnixTimestamp, RowCodecError> {
    UnixTimestamp::new(
        seconds,
        decode_u32(nanoseconds_field, i64::from(nanoseconds))?,
    )
    .map_err(RowCodecError::InvalidValue)
}

fn decode_u64_value(field: &'static str, value: &str) -> Result<u64, RowCodecError> {
    decode_u64(field, value).map_err(RowCodecError::Numeric)
}

macro_rules! nonzero_decoder {
    ($name:ident, $type:ty) => {
        fn $name(field: &'static str, value: &str) -> Result<$type, RowCodecError> {
            <$type>::new(decode_u64_value(field, value)?).map_err(RowCodecError::InvalidValue)
        }
    };
}

nonzero_decoder!(decode_state_revision, StateRevision);
nonzero_decoder!(decode_record_revision, RecordRevision);
nonzero_decoder!(decode_fencing_token, FencingToken);
nonzero_decoder!(decode_inode_generation, InodeGeneration);
nonzero_decoder!(decode_qid_path, QidPath);
nonzero_decoder!(decode_directory_generation, DirectoryGeneration);
nonzero_decoder!(decode_lock_generation, LockGeneration);

fn decode_u32(field: &'static str, value: i64) -> Result<u32, RowCodecError> {
    u32::try_from(value).map_err(|_| RowCodecError::IntegerRange { field, value })
}

fn decode_u16(field: &'static str, value: i64) -> Result<u16, RowCodecError> {
    u16::try_from(value).map_err(|_| RowCodecError::IntegerRange { field, value })
}

fn fixed_bytes<const N: usize>(
    field: &'static str,
    bytes: Vec<u8>,
) -> Result<[u8; N], RowCodecError> {
    let actual = bytes.len();
    bytes.try_into().map_err(|_| RowCodecError::FixedWidth {
        field,
        expected: N,
        actual,
    })
}

fn required<'a, T>(
    record: &'static str,
    field: &'static str,
    value: &'a Option<T>,
) -> Result<&'a T, RowCodecError> {
    value
        .as_ref()
        .ok_or(RowCodecError::MissingField { record, field })
}

fn require_inode_shape(record: &'static str, valid: bool) -> Result<(), RowCodecError> {
    if valid {
        Ok(())
    } else {
        Err(RowCodecError::InvalidShape {
            record: "inode",
            detail: record,
        })
    }
}

fn content_values_absent(row: &InodeRow) -> bool {
    row.content_file_id.is_none()
        && row.content_context_id.is_none()
        && row.data_generation.is_none()
        && row.content_generation.is_none()
        && row.content_logical_size.is_none()
        && row.content_manifest_key.is_none()
        && row.content_manifest_digest.is_none()
        && row.content_storage_method.is_none()
}

fn no_kind_specific_values(row: &InodeRow) -> bool {
    content_values_absent(row)
        && row.directory_generation.is_none()
        && row.directory_parent_inode_id.is_none()
        && row.symlink_target.is_none()
        && row.device_major.is_none()
        && row.device_minor.is_none()
}

fn only_directory_generation(row: &InodeRow) -> bool {
    content_values_absent(row)
        && row.directory_generation.is_some()
        && row.directory_parent_inode_id.is_some()
        && row.symlink_target.is_none()
        && row.device_major.is_none()
        && row.device_minor.is_none()
}

fn only_symlink_target(row: &InodeRow) -> bool {
    content_values_absent(row)
        && row.directory_generation.is_none()
        && row.directory_parent_inode_id.is_none()
        && row.symlink_target.is_some()
        && row.device_major.is_none()
        && row.device_minor.is_none()
}

fn only_device_numbers(row: &InodeRow) -> bool {
    content_values_absent(row)
        && row.directory_generation.is_none()
        && row.directory_parent_inode_id.is_none()
        && row.symlink_target.is_none()
        && row.device_major.is_some()
        && row.device_minor.is_some()
}

/// A primitive PostgreSQL row could not be converted losslessly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RowCodecError {
    FixedWidth {
        field: &'static str,
        expected: usize,
        actual: usize,
    },
    IntegerRange {
        field: &'static str,
        value: i64,
    },
    UnknownTag {
        field: &'static str,
        tag: i16,
    },
    MissingField {
        record: &'static str,
        field: &'static str,
    },
    InvalidShape {
        record: &'static str,
        detail: &'static str,
    },
    Numeric(NumericCodecError),
    Bounded(BoundedValueError),
    InvalidValue(InvalidValue),
    InvalidObjectKey(InvalidObjectKey),
    InvalidContentRef(InvalidContentRef),
    InvalidRecord(RecordValidationError),
    Limit(StateLimitError),
}

impl fmt::Display for RowCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FixedWidth {
                field,
                expected,
                actual,
            } => write!(formatter, "{field} has {actual} bytes, expected {expected}"),
            Self::IntegerRange { field, value } => {
                write!(
                    formatter,
                    "{field} value {value} is outside its unsigned range"
                )
            }
            Self::UnknownTag { field, tag } => write!(formatter, "unknown {field} tag {tag}"),
            Self::MissingField { record, field } => {
                write!(formatter, "{record} row is missing {field}")
            }
            Self::InvalidShape { record, detail } => {
                write!(formatter, "invalid {record} row shape: {detail}")
            }
            Self::Numeric(error) => error.fmt(formatter),
            Self::Bounded(error) => error.fmt(formatter),
            Self::InvalidValue(error) => error.fmt(formatter),
            Self::InvalidObjectKey(error) => error.fmt(formatter),
            Self::InvalidContentRef(error) => error.fmt(formatter),
            Self::InvalidRecord(error) => error.fmt(formatter),
            Self::Limit(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for RowCodecError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Numeric(error) => Some(error),
            Self::Bounded(error) => Some(error),
            Self::InvalidValue(error) => Some(error),
            Self::InvalidObjectKey(error) => Some(error),
            Self::InvalidContentRef(error) => Some(error),
            Self::InvalidRecord(error) => Some(error),
            Self::Limit(error) => Some(error),
            Self::FixedWidth { .. }
            | Self::IntegerRange { .. }
            | Self::UnknownTag { .. }
            | Self::MissingField { .. }
            | Self::InvalidShape { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn revision() -> RecordRevision {
        RecordRevision::new(17).unwrap()
    }

    fn times() -> InodeTimes {
        InodeTimes {
            accessed: UnixTimestamp::new(i64::MIN, 0).unwrap(),
            modified: UnixTimestamp::new(-1, 1).unwrap(),
            changed: UnixTimestamp::new(0, 999_999_999).unwrap(),
            created: UnixTimestamp::new(i64::MAX, 42).unwrap(),
        }
    }

    fn inode(
        inode_id: InodeId,
        logical_size: u64,
        data: InodeData,
        limits: StateLimits,
    ) -> InodeRecord {
        let owner = PrincipalId::new(b"owner".to_vec(), limits).unwrap();
        let group = GroupId::new(b"group".to_vec(), limits).unwrap();
        if data.kind() == InodeKind::RegularFile {
            InodeRecord::new_regular(
                inode_id,
                QidPath::new(1).unwrap(),
                revision(),
                0o6754,
                owner,
                group,
                times(),
                logical_size,
                u64::MAX,
                InodeGeneration::new(u64::MAX).unwrap(),
                w9pt_fs_storage::ContentContextId::new(*inode_id.as_bytes()),
                data,
            )
            .unwrap()
        } else {
            InodeRecord::new(
                inode_id,
                QidPath::new(1).unwrap(),
                revision(),
                0o6754,
                owner,
                group,
                times(),
                logical_size,
                u64::MAX,
                InodeGeneration::new(u64::MAX).unwrap(),
                data,
            )
            .unwrap()
        }
    }

    fn all_records(limits: StateLimits) -> Vec<(RecordKey, StateRecord)> {
        let filesystem_id = FilesystemId::from_u128(1);
        let mut records = Vec::new();
        records.push((
            RecordKey::Filesystem(filesystem_id),
            StateRecord::Filesystem(
                FilesystemRecord::new(
                    filesystem_id,
                    StateRevision::new(u64::MAX).unwrap(),
                    revision(),
                    InodeId::from_u128(2),
                    QidPath::new(u64::MAX).unwrap(),
                    DirectoryCookie::new(u64::MAX),
                    u64::MAX,
                )
                .unwrap(),
            ),
        ));

        let content_file_id = FileId::from_u128(3);
        let content = ContentRef::from_persisted(
            content_file_id,
            u64::MAX,
            3,
            ObjectKey::new("private/manifest").unwrap(),
            Digest::new([0xa5; 32]),
            StorageMethod::BlockSplit,
        )
        .unwrap();
        let inode_data = [
            (
                10,
                0,
                InodeData::RegularFile {
                    content_file_id,
                    content: None,
                    data_generation: 0,
                },
            ),
            (
                11,
                3,
                InodeData::RegularFile {
                    content_file_id,
                    content: Some(content),
                    data_generation: u64::MAX,
                },
            ),
            (
                12,
                0,
                InodeData::Directory {
                    generation: DirectoryGeneration::new(u64::MAX).unwrap(),
                    parent_inode_id: InodeId::from_u128(12),
                },
            ),
            (
                13,
                4,
                InodeData::Symlink {
                    target: SymlinkTarget::new(b"dest".to_vec(), limits).unwrap(),
                },
            ),
            (
                14,
                0,
                InodeData::CharacterDevice(DeviceNumbers {
                    major: u32::MAX,
                    minor: 0,
                }),
            ),
            (
                15,
                0,
                InodeData::BlockDevice(DeviceNumbers {
                    major: 0,
                    minor: u32::MAX,
                }),
            ),
            (16, 0, InodeData::Fifo),
            (17, 0, InodeData::Socket),
        ];
        for (id, logical_size, data) in inode_data {
            let inode_id = InodeId::from_u128(id);
            records.push((
                RecordKey::Inode(filesystem_id, inode_id),
                StateRecord::Inode(inode(inode_id, logical_size, data, limits)),
            ));
        }

        let parent = InodeId::from_u128(20);
        let name = EntryName::new(b"entry".to_vec(), limits).unwrap();
        records.push((
            RecordKey::DirectoryEntry(filesystem_id, parent, name.clone()),
            StateRecord::DirectoryEntry(
                DirectoryEntryRecord::new(
                    parent,
                    name,
                    DirectoryCookie::new(u64::MAX),
                    InodeId::from_u128(21),
                    revision(),
                )
                .unwrap(),
            ),
        ));
        let open_id = OpenId::from_u128(22);
        records.push((
            RecordKey::Open(filesystem_id, open_id),
            StateRecord::Open(OpenRecord::new(
                open_id,
                InodeId::from_u128(23),
                ClientIncarnationId::from_u128(24),
                OpenAccess::DirectoryRead,
                true,
                InodeGeneration::new(u64::MAX).unwrap(),
                revision(),
            )),
        ));
        records.push((
            RecordKey::OpenPin(filesystem_id, InodeId::from_u128(23), open_id),
            StateRecord::OpenPin(OpenPinRecord::new(
                InodeId::from_u128(23),
                open_id,
                revision(),
            )),
        ));
        records.push((
            RecordKey::Orphan(filesystem_id, InodeId::from_u128(25)),
            StateRecord::Orphan(
                OrphanRecord::new(
                    InodeId::from_u128(25),
                    u64::MAX,
                    StateRevision::new(u64::MAX).unwrap(),
                    revision(),
                )
                .unwrap(),
            ),
        ));
        let lock_id = LockId::from_u128(26);
        records.push((
            RecordKey::Lock(filesystem_id, InodeId::from_u128(27), lock_id),
            StateRecord::Lock(LockRecord::new(
                lock_id,
                InodeId::from_u128(27),
                LockRange::finite(u64::MAX - 1, u64::MAX).unwrap(),
                LockKind::Exclusive,
                LockOwner::new(ClientIncarnationId::from_u128(28), OpenId::from_u128(29)),
                LockGeneration::new(u64::MAX).unwrap(),
                revision(),
            )),
        ));
        let xattr_name = XattrName::new(b"user.name".to_vec(), limits).unwrap();
        records.push((
            RecordKey::Xattr(filesystem_id, InodeId::from_u128(30), xattr_name.clone()),
            StateRecord::Xattr(XattrRecord::new(
                InodeId::from_u128(30),
                xattr_name.clone(),
                XattrValue::new(Vec::new(), limits).unwrap(),
                revision(),
            )),
        ));
        let staging_id = XattrStagingId::from_u128(31);
        records.push((
            RecordKey::XattrStaging(filesystem_id, staging_id),
            StateRecord::XattrStaging(
                XattrStagingRecord::new(
                    staging_id,
                    InodeId::from_u128(30),
                    xattr_name,
                    3,
                    XattrValue::new(b"xy".to_vec(), limits).unwrap(),
                    revision(),
                    limits,
                )
                .unwrap(),
            ),
        ));
        let mutation_id = MutationId::from_u128(32);
        records.push((
            RecordKey::Mutation(filesystem_id, mutation_id),
            StateRecord::Mutation(MutationRecord::new(
                filesystem_id,
                mutation_id,
                RequestFingerprint::new([0x5a; 32]),
                ClientIncarnationId::from_u128(33),
                WriterScopeId::from_u128(34),
                WriterIncarnationId::from_u128(35),
                FencingToken::new(u64::MAX).unwrap(),
                MutationResult::new(
                    MutationResultKind::new(u16::MAX).unwrap(),
                    ResultFormatVersion::new(u16::MAX).unwrap(),
                    b"result".to_vec(),
                    limits,
                )
                .unwrap(),
                StateRevision::new(u64::MAX).unwrap(),
                MutationRetention::new(u64::MAX),
                revision(),
            )),
        ));
        records.push((
            RecordKey::WriterLease(filesystem_id, WriterScopeId::from_u128(36)),
            StateRecord::WriterLease(WriterLeaseRecord::new(
                filesystem_id,
                WriterScopeId::from_u128(36),
                WriterIncarnationId::from_u128(37),
                LeaseId::from_u128(38),
                LeaseDeadline::new(u64::MAX),
                FencingToken::new(u64::MAX).unwrap(),
                revision(),
            )),
        ));
        records
    }

    #[test]
    fn every_record_and_inode_variant_round_trips_losslessly() {
        let limits = StateLimits::default();
        for (key, record) in all_records(limits) {
            let row = SqlStateRecord::encode(&key, &record).unwrap();
            assert_eq!(row.decode(limits), Ok((key, record)));
        }
    }

    #[test]
    fn through_eof_and_every_small_enum_tag_round_trip() {
        let limits = StateLimits::default();
        let filesystem_id = FilesystemId::from_u128(1);
        for access in [
            OpenAccess::ReadOnly,
            OpenAccess::WriteOnly,
            OpenAccess::ReadWrite,
            OpenAccess::DirectoryRead,
        ] {
            let open_id = OpenId::from_u128(u128::from(encode_open_access(access) as u16));
            let key = RecordKey::Open(filesystem_id, open_id);
            let record = StateRecord::Open(OpenRecord::new(
                open_id,
                InodeId::from_u128(2),
                ClientIncarnationId::from_u128(3),
                access,
                false,
                InodeGeneration::new(1).unwrap(),
                revision(),
            ));
            assert_eq!(
                SqlStateRecord::encode(&key, &record)
                    .unwrap()
                    .decode(limits),
                Ok((key, record))
            );
        }
        for kind in [LockKind::Shared, LockKind::Exclusive] {
            let lock_id = LockId::from_u128(u128::from(encode_lock_kind(kind) as u16));
            let inode_id = InodeId::from_u128(4);
            let key = RecordKey::Lock(filesystem_id, inode_id, lock_id);
            let record = StateRecord::Lock(LockRecord::new(
                lock_id,
                inode_id,
                LockRange::through_eof(u64::MAX),
                kind,
                LockOwner::new(ClientIncarnationId::from_u128(5), OpenId::from_u128(6)),
                LockGeneration::new(1).unwrap(),
                revision(),
            ));
            assert_eq!(
                SqlStateRecord::encode(&key, &record)
                    .unwrap()
                    .decode(limits),
                Ok((key, record))
            );
        }
    }

    #[test]
    fn malformed_width_tags_and_optional_content_are_rejected() {
        let limits = StateLimits::default();
        let (key, record) = all_records(limits)
            .into_iter()
            .find(|(_, record)| {
                matches!(
                    record,
                    StateRecord::Inode(inode) if inode.content().is_some()
                )
            })
            .unwrap();
        let SqlStateRecord::Inode(mut row) = SqlStateRecord::encode(&key, &record).unwrap() else {
            unreachable!();
        };
        row.content_manifest_digest = None;
        assert!(matches!(
            SqlStateRecord::Inode(row).decode(limits),
            Err(RowCodecError::InvalidShape { .. })
        ));

        let SqlStateRecord::Inode(mut row) = SqlStateRecord::encode(&key, &record).unwrap() else {
            unreachable!();
        };
        row.kind = 99;
        assert!(matches!(
            SqlStateRecord::Inode(row).decode(limits),
            Err(RowCodecError::UnknownTag {
                field: "inode kind",
                tag: 99
            })
        ));

        let SqlStateRecord::Inode(mut row) = SqlStateRecord::encode(&key, &record).unwrap() else {
            unreachable!();
        };
        row.inode_id.pop();
        assert!(matches!(
            SqlStateRecord::Inode(row).decode(limits),
            Err(RowCodecError::FixedWidth {
                field: "inode_id",
                expected: 16,
                actual: 15
            })
        ));
    }

    #[test]
    fn content_summary_and_receiver_limits_are_revalidated() {
        let limits = StateLimits::default();
        let (key, record) = all_records(limits)
            .into_iter()
            .find(|(_, record)| {
                matches!(
                    record,
                    StateRecord::Inode(inode) if inode.content().is_some()
                )
            })
            .unwrap();
        let SqlStateRecord::Inode(mut row) = SqlStateRecord::encode(&key, &record).unwrap() else {
            unreachable!();
        };
        row.content_logical_size = Some("4".to_owned());
        assert!(matches!(
            SqlStateRecord::Inode(row).decode(limits),
            Err(RowCodecError::InvalidRecord(
                RecordValidationError::ContentSizeMismatch { .. }
            ))
        ));

        let loose = StateLimits::new(w9pt_fs_state::StateLimitValues {
            max_principal_bytes: limits.max_principal_bytes() + 1,
            ..limits.values()
        })
        .unwrap();
        let inode_id = InodeId::from_u128(80);
        let oversized = InodeRecord::new(
            inode_id,
            QidPath::new(1).unwrap(),
            revision(),
            0o644,
            PrincipalId::new(vec![b'p'; loose.max_principal_bytes()], loose).unwrap(),
            GroupId::new(b"g".to_vec(), loose).unwrap(),
            times(),
            0,
            1,
            InodeGeneration::new(1).unwrap(),
            InodeData::Fifo,
        )
        .unwrap();
        let key = RecordKey::Inode(FilesystemId::from_u128(1), inode_id);
        let row = SqlStateRecord::encode(&key, &StateRecord::Inode(oversized)).unwrap();
        assert!(matches!(
            row.decode(limits),
            Err(RowCodecError::Bounded(BoundedValueError::Limit(_)))
        ));
    }

    #[test]
    fn mismatched_key_and_record_are_rejected_before_encoding() {
        let limits = StateLimits::default();
        let (key, record) = all_records(limits)
            .into_iter()
            .find(|(_, record)| matches!(record, StateRecord::Open(_)))
            .unwrap();
        let wrong_key = match key {
            RecordKey::Open(filesystem_id, _) => {
                RecordKey::Open(filesystem_id, OpenId::from_u128(999))
            }
            _ => unreachable!(),
        };
        assert!(matches!(
            SqlStateRecord::encode(&wrong_key, &record),
            Err(RowCodecError::InvalidRecord(
                RecordValidationError::KeyIdentityMismatch { .. }
            ))
        ));
    }
}
