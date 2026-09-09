//! One-revision directory enumeration with persistent cookies.

use w9pt::{
    FilesystemError, LinuxErrno, Qid,
    filesystem::{OpenHandle, RequestContext},
    protocol::{DirectoryEntry, QidType},
};
use w9pt_fs_state::{
    ClientIncarnationId, DirectoryCookie, DirectoryPage, FilesystemStateStore, InodeKind,
    OpenAccess, ReadBatch, ReadConsistency, ReadOutcome, ReadQuery, ReadResult, ScanBounds,
    StateRecord,
};

use crate::{
    AccessRequirements, EngineLimits, ExportGrant, ExportPolicy, ExportPolicyRequest,
    authorization_client_error, check_directory_search, check_inode_access, open_id_from_handle,
};

use super::ReadOperationError;

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_readdir<S, P>(
    state: &S,
    policy: &P,
    limits: EngineLimits,
    context: RequestContext,
    client_incarnation: ClientIncarnationId,
    open: OpenHandle,
    offset: u64,
    count: u32,
) -> Result<Vec<DirectoryEntry>, ReadOperationError<S::Error, P::Error>>
where
    S: FilesystemStateStore,
    P: ExportPolicy,
{
    let grant = policy
        .resolve(ExportPolicyRequest::new(context))
        .await
        .map_err(ReadOperationError::Policy)?;
    let open_id = open_id_from_handle(open);
    let first = ReadBatch::new(
        grant.filesystem_id(),
        ReadConsistency::LatestLinearizable,
        vec![ReadQuery::Filesystem, ReadQuery::Open(open_id)],
        state.contract().limits(),
    )
    .map_err(ReadOperationError::MalformedRead)?;
    let first = read_snapshot(state, first).await?;
    validate_filesystem(&first.results()[0], &grant)?;
    let inode_id = resolve_directory_open(&first.results()[1], open_id, client_incarnation)?;

    let state_limits = state.contract().limits();
    let max_items = limits
        .max_directory_page_entries()
        .min(state_limits.max_scan_items());
    let max_bytes = limits
        .max_directory_page_bytes()
        .min(state_limits.max_scan_bytes());
    let bounds = ScanBounds::new(max_items, max_bytes, state_limits)
        .map_err(|_| ReadOperationError::MalformedState)?;
    let request = ReadBatch::new(
        grant.filesystem_id(),
        ReadConsistency::AtLeast(first.revision()),
        vec![
            ReadQuery::Filesystem,
            ReadQuery::Open(open_id),
            ReadQuery::Inode(inode_id),
            ReadQuery::DirectoryPage {
                parent_inode_id: inode_id,
                after: DirectoryCookie::new(offset),
                bounds,
            },
        ],
        state_limits,
    )
    .map_err(ReadOperationError::MalformedRead)?;
    let snapshot = read_snapshot(state, request).await?;
    validate_filesystem(&snapshot.results()[0], &grant)?;
    if resolve_directory_open(&snapshot.results()[1], open_id, client_incarnation)? != inode_id {
        return Err(ReadOperationError::MalformedState);
    }
    let ReadResult::Point {
        record: Some(inode),
        ..
    } = &snapshot.results()[2]
    else {
        return Err(client(LinuxErrno::EBADF));
    };
    let StateRecord::Inode(inode) = inode.as_ref() else {
        return Err(ReadOperationError::MalformedState);
    };
    if inode.inode_id() != inode_id || inode.kind() != InodeKind::Directory {
        return Err(client(LinuxErrno::ENOTDIR));
    }
    check_directory_search(&grant, inode)
        .and_then(|()| check_inode_access(&grant, inode, AccessRequirements::READ))
        .map_err(authorization_client_error)
        .map_err(ReadOperationError::Client)?;
    let ReadResult::DirectoryPage(page) = &snapshot.results()[3] else {
        return Err(ReadOperationError::MalformedState);
    };
    convert_page(page, count, limits)
}

async fn read_snapshot<S, P>(
    state: &S,
    request: ReadBatch,
) -> Result<w9pt_fs_state::StateSnapshot, ReadOperationError<S::Error, P>>
where
    S: FilesystemStateStore,
{
    match state
        .read(request)
        .await
        .map_err(ReadOperationError::State)?
    {
        ReadOutcome::Snapshot(snapshot) => Ok(snapshot),
        ReadOutcome::RevisionUnavailable { .. } => Err(ReadOperationError::RevisionUnavailable),
        ReadOutcome::MalformedRequest(error) => Err(ReadOperationError::MalformedRead(error)),
        ReadOutcome::ScanBoundTooSmall { .. } => Err(client(LinuxErrno::ERANGE)),
    }
}

fn resolve_directory_open<S, P>(
    result: &ReadResult,
    expected_open: w9pt_fs_state::OpenId,
    expected_client: ClientIncarnationId,
) -> Result<w9pt_fs_state::InodeId, ReadOperationError<S, P>> {
    let ReadResult::Point {
        record: Some(record),
        ..
    } = result
    else {
        return Err(client(LinuxErrno::EBADF));
    };
    let StateRecord::Open(open) = record.as_ref() else {
        return Err(ReadOperationError::MalformedState);
    };
    if open.open_id() != expected_open || open.client_incarnation() != expected_client {
        return Err(client(LinuxErrno::EBADF));
    }
    if open.access() != OpenAccess::DirectoryRead {
        return Err(client(LinuxErrno::EBADF));
    }
    Ok(open.inode_id())
}

fn validate_filesystem<S, P>(
    result: &ReadResult,
    grant: &ExportGrant,
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

fn convert_page<S, P>(
    page: &DirectoryPage,
    count: u32,
    limits: EngineLimits,
) -> Result<Vec<DirectoryEntry>, ReadOperationError<S, P>> {
    let available = usize::try_from(count).map_err(|_| client(LinuxErrno::EOVERFLOW))?;
    let mut used = 0usize;
    let mut entries = Vec::new();
    for entry in page.entries() {
        let name = String::from_utf8(entry.entry().name().as_bytes().to_vec())
            .map_err(|_| client(LinuxErrno::EIO))?;
        let encoded = 24usize
            .checked_add(name.len())
            .ok_or_else(|| client(LinuxErrno::EOVERFLOW))?;
        let next = used
            .checked_add(encoded)
            .ok_or_else(|| client(LinuxErrno::EOVERFLOW))?;
        if next > available || next > limits.max_directory_page_bytes() {
            if entries.is_empty() {
                return Err(client(LinuxErrno::ERANGE));
            }
            break;
        }
        entries.push(DirectoryEntry {
            qid: Qid::new(
                qid_type(entry.child_kind()),
                0,
                entry.child_qid_path().get(),
            ),
            offset: entry.entry().cookie().get(),
            ty: directory_type(entry.child_kind()),
            name,
        });
        used = next;
    }
    limits
        .check_directory_page_entries(entries.len())
        .map_err(|_| client(LinuxErrno::ENOMEM))?;
    Ok(entries)
}

const fn qid_type(kind: InodeKind) -> QidType {
    match kind {
        InodeKind::Directory => QidType::DIRECTORY,
        InodeKind::Symlink => QidType::SYMLINK,
        _ => QidType::FILE,
    }
}

const fn directory_type(kind: InodeKind) -> u8 {
    match kind {
        InodeKind::Fifo => 1,
        InodeKind::CharacterDevice => 2,
        InodeKind::Directory => 4,
        InodeKind::BlockDevice => 6,
        InodeKind::RegularFile => 8,
        InodeKind::Symlink => 10,
        InodeKind::Socket => 12,
    }
}

const fn client<S, P>(errno: LinuxErrno) -> ReadOperationError<S, P> {
    ReadOperationError::Client(FilesystemError::new(errno))
}

#[cfg(test)]
mod tests {
    use core::convert::Infallible;

    use super::*;
    use w9pt_fs_state::{
        DirectoryEntryRecord, DirectoryPageEntry, EntryName, InodeId, QidPath, RecordRevision,
        StateLimits,
    };

    #[test]
    fn conversion_returns_only_complete_cookie_ordered_entries() {
        let state_limits = StateLimits::default();
        let entry = DirectoryEntryRecord::new(
            InodeId::from_u128(1),
            EntryName::new(b"name".to_vec(), state_limits).unwrap(),
            DirectoryCookie::new(7),
            InodeId::from_u128(2),
            RecordRevision::new(3).unwrap(),
        )
        .unwrap();
        let page = DirectoryPage::new(
            vec![DirectoryPageEntry::new(
                entry,
                InodeKind::RegularFile,
                QidPath::new(9).unwrap(),
                RecordRevision::new(3).unwrap(),
            )],
            None,
        );
        assert!(matches!(
            convert_page::<Infallible, Infallible>(&page, 27, EngineLimits::default()),
            Err(ReadOperationError::Client(error)) if error.errno == LinuxErrno::ERANGE
        ));
        let entries =
            convert_page::<Infallible, Infallible>(&page, 28, EngineLimits::default()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].offset, 7);
        assert_eq!(entries[0].qid.path, 9);
        assert_eq!(entries[0].ty, 8);
    }
}
