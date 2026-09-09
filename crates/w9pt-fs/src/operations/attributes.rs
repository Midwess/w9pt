//! Inode attribute reads and wire conversion.

use w9pt::{
    FilesystemError, LinuxErrno,
    filesystem::{ObjectHandle, RequestContext},
    protocol::{FileAttributes, GetattrMask, Timestamp},
};
use w9pt_fs_state::{
    DeviceNumbers, FilesystemStateStore, InodeData, InodeKind, InodeRecord, ReadBatch,
    ReadConsistency, ReadOutcome, ReadQuery, ReadResult, StateRecord,
};

use crate::{
    CanonicalIdentity, EngineLimits, ExportPolicy, ExportPolicyRequest, IdentityMappingRequest,
    NumericIdentity, inode_id_from_handle, qid_from_inode,
};

use super::ReadOperationError;

pub(crate) async fn execute_getattr<S, P>(
    state: &S,
    policy: &P,
    _limits: EngineLimits,
    context: RequestContext,
    object: ObjectHandle,
    mask: GetattrMask,
) -> Result<FileAttributes, ReadOperationError<S::Error, P::Error>>
where
    S: FilesystemStateStore,
    P: ExportPolicy,
{
    if mask.bits() & !GetattrMask::ALL.bits() != 0 {
        return Err(client(LinuxErrno::EINVAL));
    }
    let grant = policy
        .resolve(ExportPolicyRequest::new(context))
        .await
        .map_err(ReadOperationError::Policy)?;
    let inode_id = inode_id_from_handle(object);
    let request = ReadBatch::new(
        grant.filesystem_id(),
        ReadConsistency::LatestLinearizable,
        vec![ReadQuery::Filesystem, ReadQuery::Inode(inode_id)],
        state.contract().limits(),
    )
    .map_err(ReadOperationError::MalformedRead)?;
    let ReadOutcome::Snapshot(snapshot) = state
        .read(request)
        .await
        .map_err(ReadOperationError::State)?
    else {
        return Err(ReadOperationError::RevisionUnavailable);
    };
    let [filesystem, inode] = snapshot.results() else {
        return Err(ReadOperationError::MalformedState);
    };
    validate_filesystem(filesystem, &grant)?;
    let ReadResult::Point {
        record: Some(inode),
        ..
    } = inode
    else {
        return Err(client(LinuxErrno::EBADF));
    };
    let StateRecord::Inode(inode) = inode.as_ref() else {
        return Err(ReadOperationError::MalformedState);
    };
    if inode.inode_id() != inode_id {
        return Err(ReadOperationError::MalformedState);
    }

    let uid = if mask.contains(GetattrMask::UID) {
        match policy
            .map_numeric_identity(IdentityMappingRequest {
                filesystem_id: grant.filesystem_id(),
                policy_generation: grant.policy_generation(),
                identity: CanonicalIdentity::Principal(inode.owner().clone()),
            })
            .await
            .map_err(ReadOperationError::Policy)?
        {
            NumericIdentity::User(uid) => uid,
            NumericIdentity::Group(_) => return Err(ReadOperationError::MalformedState),
        }
    } else {
        0
    };
    let gid = if mask.contains(GetattrMask::GID) {
        match policy
            .map_numeric_identity(IdentityMappingRequest {
                filesystem_id: grant.filesystem_id(),
                policy_generation: grant.policy_generation(),
                identity: CanonicalIdentity::Group(inode.group().clone()),
            })
            .await
            .map_err(ReadOperationError::Policy)?
        {
            NumericIdentity::Group(gid) => gid,
            NumericIdentity::User(_) => return Err(ReadOperationError::MalformedState),
        }
    } else {
        0
    };
    convert_attributes(inode, mask, uid, gid)
}

fn convert_attributes<S, P>(
    inode: &InodeRecord,
    mask: GetattrMask,
    uid: u32,
    gid: u32,
) -> Result<FileAttributes, ReadOperationError<S, P>> {
    let times = inode.times();
    let timestamp = |selected: bool, value: w9pt_fs_state::UnixTimestamp| {
        if !selected {
            return Ok(Timestamp::default());
        }
        Ok(Timestamp {
            seconds: u64::try_from(value.seconds()).map_err(|_| client(LinuxErrno::EOVERFLOW))?,
            nanoseconds: u64::from(value.nanoseconds()),
        })
    };
    let device = match inode.data() {
        InodeData::CharacterDevice(numbers) | InodeData::BlockDevice(numbers) => {
            encode_device(*numbers)
        }
        _ => 0,
    };
    let size = inode.logical_size();
    let blocks = size / 512 + u64::from(!size.is_multiple_of(512));
    Ok(FileAttributes {
        valid: mask,
        qid: qid_from_inode(inode),
        mode: kind_mode(inode.kind()) | inode.mode(),
        uid,
        gid,
        link_count: inode.link_count(),
        device,
        size,
        block_size: u64::from(w9pt_fs_storage::BLOCK_SIZE),
        blocks,
        accessed: timestamp(mask.contains(GetattrMask::ATIME), times.accessed)?,
        modified: timestamp(mask.contains(GetattrMask::MTIME), times.modified)?,
        changed: timestamp(mask.contains(GetattrMask::CTIME), times.changed)?,
        created: timestamp(mask.contains(GetattrMask::BTIME), times.created)?,
        generation: inode.inode_generation().get(),
        data_version: inode
            .data_generation()
            .map_or(0, w9pt_fs_state::DataGeneration::get),
    })
}

const fn kind_mode(kind: InodeKind) -> u32 {
    match kind {
        InodeKind::RegularFile => 0o100_000,
        InodeKind::Directory => 0o040_000,
        InodeKind::Symlink => 0o120_000,
        InodeKind::CharacterDevice => 0o020_000,
        InodeKind::BlockDevice => 0o060_000,
        InodeKind::Fifo => 0o010_000,
        InodeKind::Socket => 0o140_000,
    }
}

const fn encode_device(numbers: DeviceNumbers) -> u64 {
    (numbers.major as u64) << 32 | numbers.minor as u64
}

fn validate_filesystem<S, P>(
    result: &ReadResult,
    grant: &crate::ExportGrant,
) -> Result<(), ReadOperationError<S, P>> {
    let ReadResult::Point {
        record: Some(record),
        ..
    } = result
    else {
        return Err(ReadOperationError::MalformedState);
    };
    let StateRecord::Filesystem(filesystem) = record.as_ref() else {
        return Err(ReadOperationError::MalformedState);
    };
    if filesystem.filesystem_id() != grant.filesystem_id()
        || filesystem.root_inode_id() != grant.root_inode_id()
        || filesystem.policy_generation() != grant.policy_generation()
    {
        return Err(client(LinuxErrno::EAGAIN));
    }
    Ok(())
}

const fn client<S, P>(errno: LinuxErrno) -> ReadOperationError<S, P> {
    ReadOperationError::Client(FilesystemError::new(errno))
}

#[cfg(test)]
mod tests {
    use core::convert::Infallible;

    use super::*;
    use w9pt_fs_state::{
        DirectoryGeneration, GroupId, InodeGeneration, InodeTimes, PrincipalId, QidPath,
        RecordRevision, StateLimits, UnixTimestamp,
    };

    fn directory(timestamp: UnixTimestamp) -> InodeRecord {
        let state_limits = StateLimits::default();
        let inode_id = w9pt_fs_state::InodeId::from_u128(1);
        InodeRecord::new(
            inode_id,
            QidPath::new(2).unwrap(),
            RecordRevision::new(3).unwrap(),
            0o2750,
            PrincipalId::new(b"owner".to_vec(), state_limits).unwrap(),
            GroupId::new(b"group".to_vec(), state_limits).unwrap(),
            InodeTimes {
                accessed: timestamp,
                modified: timestamp,
                changed: timestamp,
                created: timestamp,
            },
            0,
            1,
            InodeGeneration::new(4).unwrap(),
            InodeData::Directory {
                generation: DirectoryGeneration::new(5).unwrap(),
                parent_inode_id: inode_id,
            },
        )
        .unwrap()
    }

    #[test]
    fn conversion_preserves_requested_fields_kind_and_generations() {
        let inode = directory(UnixTimestamp::new(7, 8).unwrap());
        let mask = GetattrMask::MODE | GetattrMask::UID | GetattrMask::GID | GetattrMask::ATIME;
        let attributes =
            convert_attributes::<Infallible, Infallible>(&inode, mask, 11, 12).unwrap();
        assert_eq!(attributes.valid, mask);
        assert_eq!(attributes.mode, 0o042750);
        assert_eq!(attributes.uid, 11);
        assert_eq!(attributes.gid, 12);
        assert_eq!(attributes.accessed.seconds, 7);
        assert_eq!(attributes.generation, 4);
        assert_eq!(attributes.data_version, 0);
        assert_eq!(attributes.qid.path, 2);
    }

    #[test]
    fn selected_negative_timestamp_is_an_overflow() {
        let inode = directory(UnixTimestamp::new(-1, 0).unwrap());
        assert!(matches!(
            convert_attributes::<Infallible, Infallible>(&inode, GetattrMask::ATIME, 0, 0),
            Err(ReadOperationError::Client(error)) if error.errno == LinuxErrno::EOVERFLOW
        ));
    }
}
