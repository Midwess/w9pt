//! Symbolic-link reads with checked wire conversion.

use w9pt::{
    FilesystemError, LinuxErrno,
    filesystem::{ObjectHandle, RequestContext},
};
use w9pt_fs_state::{
    FilesystemStateStore, InodeData, ReadBatch, ReadConsistency, ReadOutcome, ReadQuery,
    ReadResult, StateRecord,
};

use crate::{
    AccessRequirements, EngineLimits, ExportPolicy, ExportPolicyRequest,
    authorization_client_error, check_inode_access, inode_id_from_handle,
};

use super::ReadOperationError;

pub(crate) async fn execute_readlink<S, P>(
    state: &S,
    policy: &P,
    limits: EngineLimits,
    context: RequestContext,
    object: ObjectHandle,
) -> Result<String, ReadOperationError<S::Error, P::Error>>
where
    S: FilesystemStateStore,
    P: ExportPolicy,
{
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
    let ReadResult::Point {
        record: Some(filesystem),
        ..
    } = filesystem
    else {
        return Err(ReadOperationError::MalformedState);
    };
    let StateRecord::Filesystem(filesystem) = filesystem.as_ref() else {
        return Err(ReadOperationError::MalformedState);
    };
    if filesystem.filesystem_id() != grant.filesystem_id()
        || filesystem.root_inode_id() != grant.root_inode_id()
        || filesystem.policy_generation() != grant.policy_generation()
    {
        return Err(client(LinuxErrno::EAGAIN));
    }
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
    check_inode_access(&grant, inode, AccessRequirements::READ)
        .map_err(authorization_client_error)
        .map_err(ReadOperationError::Client)?;
    let InodeData::Symlink { target } = inode.data() else {
        return Err(client(LinuxErrno::EINVAL));
    };
    target_string(target.as_bytes(), limits)
}

fn target_string<S, P>(
    target: &[u8],
    limits: EngineLimits,
) -> Result<String, ReadOperationError<S, P>> {
    limits
        .check_result_bytes(target.len())
        .map_err(|_| client(LinuxErrno::ERANGE))?;
    String::from_utf8(target.to_vec()).map_err(|_| client(LinuxErrno::EIO))
}

const fn client<S, P>(errno: LinuxErrno) -> ReadOperationError<S, P> {
    ReadOperationError::Client(FilesystemError::new(errno))
}

#[cfg(test)]
mod tests {
    use core::convert::Infallible;

    use super::*;

    #[test]
    fn target_conversion_is_exact_and_rejects_non_utf8_state() {
        assert_eq!(
            target_string::<Infallible, Infallible>(b"../target", EngineLimits::default()).unwrap(),
            "../target"
        );
        assert!(matches!(
            target_string::<Infallible, Infallible>(&[0xff], EngineLimits::default()),
            Err(ReadOperationError::Client(error)) if error.errno == LinuxErrno::EIO
        ));
    }
}
