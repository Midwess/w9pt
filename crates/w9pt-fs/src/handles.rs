//! Portable lossless handles and persisted QID construction.

use core::fmt;

use w9pt::{
    Qid,
    filesystem::{ObjectHandle, OpenHandle},
    protocol::QidType,
};
use w9pt_fs_state::{
    ClientIncarnationId, InodeId, InodeKind, InodeRecord, OpenId, OpenRecord, RecordKey,
    StateRecord,
};

use crate::ExportGrant;

/// Encodes a complete stable inode identity into its portable protocol handle.
pub const fn object_handle(inode_id: InodeId) -> ObjectHandle {
    ObjectHandle::new(u128::from_be_bytes(*inode_id.as_bytes()))
}

/// Decodes a complete portable object handle without hashing or truncation.
pub const fn inode_id_from_handle(handle: ObjectHandle) -> InodeId {
    InodeId::new(handle.get().to_be_bytes())
}

/// Encodes a complete stable open identity into its portable protocol handle.
pub const fn open_handle(open_id: OpenId) -> OpenHandle {
    OpenHandle::new(u128::from_be_bytes(*open_id.as_bytes()))
}

/// Decodes a complete portable open handle without process-local state.
pub const fn open_id_from_handle(handle: OpenHandle) -> OpenId {
    OpenId::new(handle.get().to_be_bytes())
}

/// Constructs a wire QID only from persisted inode kind and QID-path state.
pub const fn qid_from_inode(inode: &InodeRecord) -> Qid {
    let ty = match inode.kind() {
        InodeKind::Directory => QidType::DIRECTORY,
        InodeKind::Symlink => QidType::SYMLINK,
        InodeKind::RegularFile
        | InodeKind::CharacterDevice
        | InodeKind::BlockDevice
        | InodeKind::Fifo
        | InodeKind::Socket => QidType::FILE,
    };
    Qid::new(ty, 0, inode.qid_path().get())
}

/// Resolves a point-read result against its complete object handle and export.
pub fn resolve_inode_record<'a>(
    grant: &ExportGrant,
    handle: ObjectHandle,
    key: &RecordKey,
    record: Option<&'a StateRecord>,
) -> Result<&'a InodeRecord, HandleResolutionError> {
    let inode_id = inode_id_from_handle(handle);
    match key {
        RecordKey::Inode(filesystem_id, key_inode_id)
            if *filesystem_id != grant.filesystem_id() =>
        {
            return Err(HandleResolutionError::ExportMismatch);
        }
        RecordKey::Inode(_, key_inode_id) if *key_inode_id != inode_id => {
            return Err(HandleResolutionError::IdentityMismatch);
        }
        RecordKey::Inode(_, _) => {}
        _ => return Err(HandleResolutionError::MalformedRecordKind),
    }
    match record {
        Some(StateRecord::Inode(inode)) if inode.inode_id() == inode_id => Ok(inode),
        Some(StateRecord::Inode(_)) => Err(HandleResolutionError::IdentityMismatch),
        Some(_) => Err(HandleResolutionError::MalformedRecordKind),
        None => Err(HandleResolutionError::UnknownObject),
    }
}

/// Resolves a point-read result against its complete open handle, export, and client.
pub fn resolve_open_record<'a>(
    grant: &ExportGrant,
    client_incarnation: ClientIncarnationId,
    handle: OpenHandle,
    key: &RecordKey,
    record: Option<&'a StateRecord>,
) -> Result<&'a OpenRecord, HandleResolutionError> {
    let open_id = open_id_from_handle(handle);
    match key {
        RecordKey::Open(filesystem_id, key_open_id) if *filesystem_id != grant.filesystem_id() => {
            return Err(HandleResolutionError::ExportMismatch);
        }
        RecordKey::Open(_, key_open_id) if *key_open_id != open_id => {
            return Err(HandleResolutionError::IdentityMismatch);
        }
        RecordKey::Open(_, _) => {}
        _ => return Err(HandleResolutionError::MalformedRecordKind),
    }
    match record {
        Some(StateRecord::Open(open)) if open.open_id() != open_id => {
            Err(HandleResolutionError::IdentityMismatch)
        }
        Some(StateRecord::Open(open)) if open.client_incarnation() != client_incarnation => {
            Err(HandleResolutionError::ClientMismatch)
        }
        Some(StateRecord::Open(open)) => Ok(open),
        Some(_) => Err(HandleResolutionError::MalformedRecordKind),
        None => Err(HandleResolutionError::UnknownOpen),
    }
}

/// Checked failure resolving a portable handle through authoritative state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HandleResolutionError {
    /// No inode exists for the decoded object identity in this export.
    UnknownObject,
    /// No open exists for the decoded open identity in this export.
    UnknownOpen,
    /// A result key belongs to a filesystem other than the resolved export.
    ExportMismatch,
    /// A result key or record does not match the complete decoded handle identity.
    IdentityMismatch,
    /// A portable open belongs to another client incarnation.
    ClientMismatch,
    /// An adapter returned a record family incompatible with the lookup.
    MalformedRecordKind,
}

impl fmt::Display for HandleResolutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "portable handle resolution failed: {self:?}")
    }
}

impl std::error::Error for HandleResolutionError {}

#[cfg(test)]
mod tests {
    use super::*;
    use w9pt::filesystem::CapabilitySet;
    use w9pt_fs_state::{
        DirectoryGeneration, GroupId, InodeData, InodeGeneration, InodeTimes, PrincipalId, QidPath,
        RecordRevision, StateLimits, UnixTimestamp,
    };

    fn inode(inode_id: InodeId, qid_path: u64) -> InodeRecord {
        let limits = StateLimits::default();
        let timestamp = UnixTimestamp::new(0, 0).unwrap();
        InodeRecord::new(
            inode_id,
            QidPath::new(qid_path).unwrap(),
            RecordRevision::new(1).unwrap(),
            0o755,
            PrincipalId::new(b"owner".to_vec(), limits).unwrap(),
            GroupId::new(b"group".to_vec(), limits).unwrap(),
            InodeTimes {
                accessed: timestamp,
                modified: timestamp,
                changed: timestamp,
                created: timestamp,
            },
            0,
            1,
            InodeGeneration::new(1).unwrap(),
            InodeData::Directory {
                generation: DirectoryGeneration::new(1).unwrap(),
                parent_inode_id: inode_id,
            },
        )
        .unwrap()
    }

    fn grant(filesystem_id: w9pt_fs_state::FilesystemId) -> ExportGrant {
        let limits = StateLimits::default();
        ExportGrant::new(
            filesystem_id,
            InodeId::from_u128(1),
            PrincipalId::new(b"owner".to_vec(), limits).unwrap(),
            GroupId::new(b"group".to_vec(), limits).unwrap(),
            Vec::new(),
            1,
            1,
            false,
            false,
            1,
            CapabilitySet::NONE,
            crate::EngineLimits::default(),
        )
        .unwrap()
    }

    #[test]
    fn handles_round_trip_all_bits_and_qids_use_only_persisted_paths() {
        let inode_id = InodeId::from_u128(u128::MAX - 7);
        let open_id = OpenId::from_u128(u128::MAX - 8);
        assert_eq!(inode_id_from_handle(object_handle(inode_id)), inode_id);
        assert_eq!(open_id_from_handle(open_handle(open_id)), open_id);
        let inode = inode(inode_id, 9);
        assert_eq!(qid_from_inode(&inode), Qid::new(QidType::DIRECTORY, 0, 9));
    }

    #[test]
    fn resolution_rejects_cross_export_and_mismatched_records() {
        let filesystem_id = w9pt_fs_state::FilesystemId::from_u128(1);
        let other_filesystem = w9pt_fs_state::FilesystemId::from_u128(2);
        let inode_id = InodeId::from_u128(3);
        let inode = StateRecord::Inode(inode(inode_id, 4));
        let handle = object_handle(inode_id);
        assert!(
            resolve_inode_record(
                &grant(filesystem_id),
                handle,
                &RecordKey::Inode(filesystem_id, inode_id),
                Some(&inode),
            )
            .is_ok()
        );
        assert_eq!(
            resolve_inode_record(
                &grant(filesystem_id),
                handle,
                &RecordKey::Inode(other_filesystem, inode_id),
                Some(&inode),
            ),
            Err(HandleResolutionError::ExportMismatch)
        );
        assert_eq!(
            resolve_inode_record(
                &grant(filesystem_id),
                handle,
                &RecordKey::Inode(filesystem_id, InodeId::from_u128(5)),
                Some(&inode),
            ),
            Err(HandleResolutionError::IdentityMismatch)
        );
    }
}
