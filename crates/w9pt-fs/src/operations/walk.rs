//! Bounded component walk with authoritative dot-dot traversal.

use w9pt::{
    FilesystemError, LinuxErrno,
    filesystem::{ObjectHandle, RequestContext, WalkElement, WalkResult},
};
use w9pt_fs_state::{
    DirectoryEntryRecord, EntryName, FilesystemStateStore, InodeId, InodeRecord, ReadBatch,
    ReadConsistency, ReadOutcome, ReadQuery, ReadResult, StateRecord,
};

use crate::{
    EngineLimits, ExportGrant, ExportPolicy, ExportPolicyRequest, authorization_client_error,
    check_directory_search, object_handle, qid_from_inode,
};

use super::ReadOperationError;

pub(crate) async fn execute_walk<S, P>(
    state: &S,
    policy: &P,
    limits: EngineLimits,
    context: RequestContext,
    start: ObjectHandle,
    names: Vec<String>,
) -> Result<WalkResult, ReadOperationError<S::Error, P::Error>>
where
    S: FilesystemStateStore,
    P: ExportPolicy,
{
    if names.is_empty() {
        return Err(client(LinuxErrno::EINVAL));
    }
    limits
        .check_walk_depth(names.len())
        .map_err(|_| client(LinuxErrno::ENAMETOOLONG))?;
    let grant = policy
        .resolve(ExportPolicyRequest::new(context))
        .await
        .map_err(ReadOperationError::Policy)?;
    let mut current = read_inode(
        state,
        &grant,
        crate::inode_id_from_handle(start),
        ReadConsistency::LatestLinearizable,
    )
    .await?
    .0;
    let mut elements = Vec::with_capacity(names.len());

    for name in names {
        check_directory_search(&grant, &current)
            .map_err(authorization_client_error)
            .map_err(ReadOperationError::Client)?;
        let next = if name == "." {
            current.clone()
        } else if name == ".." {
            if current.inode_id() == grant.root_inode_id() {
                current.clone()
            } else {
                let parent = current
                    .directory_parent()
                    .ok_or(ReadOperationError::MalformedState)?;
                if parent == current.inode_id() {
                    return Err(ReadOperationError::MalformedState);
                }
                read_inode(state, &grant, parent, ReadConsistency::LatestLinearizable)
                    .await?
                    .0
            }
        } else {
            validate_component(&name, state.contract().limits())?;
            match read_child(state, &grant, &name, current.inode_id(), limits).await? {
                Some(child) => child,
                None if !elements.is_empty() => break,
                None => return Err(client(LinuxErrno::ENOENT)),
            }
        };
        elements.push(WalkElement {
            object: object_handle(next.inode_id()),
            qid: qid_from_inode(&next),
        });
        current = next;
    }
    Ok(WalkResult { elements })
}

fn validate_component<S, P>(
    name: &str,
    state_limits: w9pt_fs_state::StateLimits,
) -> Result<(), ReadOperationError<S, P>> {
    if name.len() > state_limits.max_entry_name_bytes() {
        return Err(client(LinuxErrno::ENAMETOOLONG));
    }
    EntryName::new(name.as_bytes().to_vec(), state_limits)
        .map(|_| ())
        .map_err(|_| client(LinuxErrno::EINVAL))
}

async fn read_child<S, P>(
    state: &S,
    grant: &ExportGrant,
    name: &str,
    parent: InodeId,
    limits: EngineLimits,
) -> Result<Option<InodeRecord>, ReadOperationError<S::Error, P>>
where
    S: FilesystemStateStore,
{
    let state_limits = state.contract().limits();
    let name = EntryName::new(name.as_bytes().to_vec(), state_limits)
        .map_err(|_| client(LinuxErrno::EINVAL))?;
    for attempt in 0..=limits.max_conflict_retries() {
        let request = ReadBatch::new(
            grant.filesystem_id(),
            ReadConsistency::LatestLinearizable,
            vec![
                ReadQuery::Filesystem,
                ReadQuery::DirectoryEntry {
                    parent_inode_id: parent,
                    name: name.clone(),
                },
            ],
            state_limits,
        )
        .map_err(ReadOperationError::MalformedRead)?;
        let first_snapshot = snapshot(
            state
                .read(request)
                .await
                .map_err(ReadOperationError::State)?,
        )?;
        validate_policy(&first_snapshot.results()[0], grant)?;
        let entry = point_directory_entry(&first_snapshot.results()[1])?;
        let Some(entry) = entry else {
            return Ok(None);
        };

        let request = ReadBatch::new(
            grant.filesystem_id(),
            ReadConsistency::AtLeast(first_snapshot.revision()),
            vec![
                ReadQuery::Filesystem,
                ReadQuery::DirectoryEntry {
                    parent_inode_id: parent,
                    name: name.clone(),
                },
                ReadQuery::Inode(entry.child_inode_id()),
            ],
            state_limits,
        )
        .map_err(ReadOperationError::MalformedRead)?;
        let checked = snapshot(
            state
                .read(request)
                .await
                .map_err(ReadOperationError::State)?,
        )?;
        validate_policy(&checked.results()[0], grant)?;
        let checked_entry = point_directory_entry(&checked.results()[1])?;
        if checked_entry
            .as_ref()
            .map(DirectoryEntryRecord::child_inode_id)
            != Some(entry.child_inode_id())
        {
            if attempt == limits.max_conflict_retries() {
                return Err(client(LinuxErrno::EAGAIN));
            }
            continue;
        }
        let child =
            point_inode(&checked.results()[2])?.ok_or(ReadOperationError::MalformedState)?;
        return Ok(Some(child));
    }
    Err(client(LinuxErrno::EAGAIN))
}

async fn read_inode<S, P>(
    state: &S,
    grant: &ExportGrant,
    inode_id: InodeId,
    consistency: ReadConsistency,
) -> Result<(InodeRecord, w9pt_fs_state::StateRevision), ReadOperationError<S::Error, P>>
where
    S: FilesystemStateStore,
{
    let request = ReadBatch::new(
        grant.filesystem_id(),
        consistency,
        vec![ReadQuery::Filesystem, ReadQuery::Inode(inode_id)],
        state.contract().limits(),
    )
    .map_err(ReadOperationError::MalformedRead)?;
    let snapshot = snapshot(
        state
            .read(request)
            .await
            .map_err(ReadOperationError::State)?,
    )?;
    validate_policy(&snapshot.results()[0], grant)?;
    let inode = point_inode(&snapshot.results()[1])?.ok_or_else(|| client(LinuxErrno::EBADF))?;
    Ok((inode, snapshot.revision()))
}

fn validate_policy<S, P>(
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
        || filesystem.policy_generation() != grant.policy_generation()
        || filesystem.root_inode_id() != grant.root_inode_id()
    {
        return Err(client(LinuxErrno::EAGAIN));
    }
    Ok(())
}

fn point_inode<S, P>(result: &ReadResult) -> Result<Option<InodeRecord>, ReadOperationError<S, P>> {
    let ReadResult::Point { record, .. } = result else {
        return Err(ReadOperationError::MalformedState);
    };
    match record.as_deref() {
        Some(StateRecord::Inode(inode)) => Ok(Some(inode.clone())),
        None => Ok(None),
        Some(_) => Err(ReadOperationError::MalformedState),
    }
}

fn point_directory_entry<S, P>(
    result: &ReadResult,
) -> Result<Option<DirectoryEntryRecord>, ReadOperationError<S, P>> {
    let ReadResult::Point { record, .. } = result else {
        return Err(ReadOperationError::MalformedState);
    };
    match record.as_deref() {
        Some(StateRecord::DirectoryEntry(entry)) => Ok(Some(entry.clone())),
        None => Ok(None),
        Some(_) => Err(ReadOperationError::MalformedState),
    }
}

fn snapshot<S, P>(
    outcome: ReadOutcome,
) -> Result<w9pt_fs_state::StateSnapshot, ReadOperationError<S, P>> {
    match outcome {
        ReadOutcome::Snapshot(snapshot) => Ok(snapshot),
        ReadOutcome::RevisionUnavailable { .. } => Err(ReadOperationError::RevisionUnavailable),
        ReadOutcome::MalformedRequest(error) => Err(ReadOperationError::MalformedRead(error)),
        ReadOutcome::ScanBoundTooSmall { .. } => Err(ReadOperationError::MalformedState),
    }
}

const fn client<S, P>(errno: LinuxErrno) -> ReadOperationError<S, P> {
    ReadOperationError::Client(FilesystemError::new(errno))
}

#[cfg(test)]
mod tests {
    use core::{convert::Infallible, future::Future};

    use super::*;
    use w9pt::filesystem::{CapabilitySet, ExportId, PrincipalId};
    use w9pt::protocol::SessionId;
    use w9pt_fs_state::{
        AcquireLeaseOutcome, AcquireWriterLease, ClientIncarnationId, CommitOutcome, CommitRequest,
        DirectoryCookie, DirectoryGeneration, FilesystemId, FilesystemRecord, GroupId, InodeData,
        InodeGeneration, InodeTimes, LeaseDeadline, LeaseDuration, LeaseId, LeaseOperationId,
        ManualLeaseClock, MutationContext, MutationRetention, Precondition,
        PrincipalId as StatePrincipalId, QidPath, RecordKey, RecordRevision, RequestFingerprint,
        StateChange, StateLimits, StateRecord, StateRevision, WriterFence, WriterIncarnationId,
        WriterScopeId, WriterTopology, testing::MemoryAuthority,
    };
    use w9pt_fs_storage::{MutationId, testing::block_on};

    use crate::{
        CanonicalIdentity, IdentityMappingRequest, NumericIdentity, ReverseIdentityMappingRequest,
    };

    #[derive(Clone)]
    struct Policy(ExportGrant);

    impl ExportPolicy for Policy {
        type Error = Infallible;

        fn resolve(
            &self,
            _request: ExportPolicyRequest,
        ) -> impl Future<Output = Result<ExportGrant, Self::Error>> + Send {
            core::future::ready(Ok(self.0.clone()))
        }

        fn map_numeric_identity(
            &self,
            request: IdentityMappingRequest,
        ) -> impl Future<Output = Result<NumericIdentity, Self::Error>> + Send {
            core::future::ready(Ok(match request.identity {
                CanonicalIdentity::Principal(_) => NumericIdentity::User(1),
                CanonicalIdentity::Group(_) => NumericIdentity::Group(1),
            }))
        }

        fn map_canonical_identity(
            &self,
            request: ReverseIdentityMappingRequest,
        ) -> impl Future<Output = Result<CanonicalIdentity, Self::Error>> + Send {
            core::future::ready(Ok(match request.identity {
                NumericIdentity::User(_) => {
                    CanonicalIdentity::Principal(self.0.principal().clone())
                }
                NumericIdentity::Group(_) => {
                    CanonicalIdentity::Group(self.0.primary_group().clone())
                }
            }))
        }
    }

    #[test]
    fn walk_handles_dot_dot_root_clamping_and_nonempty_partial_success() {
        block_on(async {
            let limits = StateLimits::default();
            let engine_limits = EngineLimits::default();
            let filesystem_id = FilesystemId::from_u128(1);
            let root_id = InodeId::from_u128(2);
            let child_id = InodeId::from_u128(3);
            let nested_id = InodeId::from_u128(4);
            let authority = MemoryAuthority::new(
                WriterTopology::SerializableMultiWriter,
                limits,
                ManualLeaseClock::new(LeaseDeadline::new(0)),
            );
            let state = authority.open_client();
            let acquire = AcquireWriterLease::new(
                filesystem_id,
                LeaseOperationId::from_u128(5),
                WriterScopeId::from_u128(6),
                WriterIncarnationId::from_u128(7),
                LeaseId::from_u128(8),
                LeaseDuration::new(100).unwrap(),
                limits,
            )
            .unwrap();
            let AcquireLeaseOutcome::Granted(lease) =
                state.acquire_writer_lease(acquire).await.unwrap()
            else {
                panic!("lease not granted")
            };
            let timestamp = w9pt_fs_state::UnixTimestamp::new(0, 0).unwrap();
            let times = InodeTimes {
                accessed: timestamp,
                modified: timestamp,
                changed: timestamp,
                created: timestamp,
            };
            let owner = StatePrincipalId::new(b"owner".to_vec(), limits).unwrap();
            let group = GroupId::new(b"group".to_vec(), limits).unwrap();
            let directory = |inode_id, qid, parent| {
                InodeRecord::new(
                    inode_id,
                    QidPath::new(qid).unwrap(),
                    RecordRevision::new(1).unwrap(),
                    0o755,
                    owner.clone(),
                    group.clone(),
                    times,
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
            let filesystem = FilesystemRecord::new(
                filesystem_id,
                StateRevision::new(1).unwrap(),
                RecordRevision::new(1).unwrap(),
                root_id,
                QidPath::new(2).unwrap(),
                DirectoryCookie::new(1),
                1,
            )
            .unwrap();
            commit(
                &state,
                filesystem_id,
                lease.fence,
                9,
                vec![
                    StateChange::Insert {
                        key: RecordKey::Filesystem(filesystem_id),
                        record: StateRecord::Filesystem(filesystem),
                    },
                    StateChange::Insert {
                        key: RecordKey::Inode(filesystem_id, root_id),
                        record: StateRecord::Inode(directory(root_id, 1, root_id)),
                    },
                ],
                limits,
            )
            .await;
            let child_name = EntryName::new(b"child".to_vec(), limits).unwrap();
            commit(
                &state,
                filesystem_id,
                lease.fence,
                10,
                vec![
                    StateChange::Insert {
                        key: RecordKey::Inode(filesystem_id, child_id),
                        record: StateRecord::Inode(directory(child_id, 2, root_id)),
                    },
                    StateChange::Insert {
                        key: RecordKey::DirectoryEntry(filesystem_id, root_id, child_name.clone()),
                        record: StateRecord::DirectoryEntry(
                            w9pt_fs_state::DirectoryEntryRecord::new(
                                root_id,
                                child_name,
                                DirectoryCookie::new(1),
                                child_id,
                                RecordRevision::new(1).unwrap(),
                            )
                            .unwrap(),
                        ),
                    },
                    StateChange::AdvanceQidPath {
                        count: core::num::NonZeroU64::new(1).unwrap(),
                    },
                    StateChange::AdvanceDirectoryCookie {
                        count: core::num::NonZeroU64::new(1).unwrap(),
                    },
                    StateChange::BumpDirectoryGeneration(root_id),
                ],
                limits,
            )
            .await;
            let nested_name = EntryName::new(b"nested".to_vec(), limits).unwrap();
            commit(
                &state,
                filesystem_id,
                lease.fence,
                11,
                vec![
                    StateChange::Insert {
                        key: RecordKey::Inode(filesystem_id, nested_id),
                        record: StateRecord::Inode(directory(nested_id, 3, child_id)),
                    },
                    StateChange::Insert {
                        key: RecordKey::DirectoryEntry(
                            filesystem_id,
                            child_id,
                            nested_name.clone(),
                        ),
                        record: StateRecord::DirectoryEntry(
                            w9pt_fs_state::DirectoryEntryRecord::new(
                                child_id,
                                nested_name,
                                DirectoryCookie::new(2),
                                nested_id,
                                RecordRevision::new(1).unwrap(),
                            )
                            .unwrap(),
                        ),
                    },
                    StateChange::AdvanceQidPath {
                        count: core::num::NonZeroU64::new(1).unwrap(),
                    },
                    StateChange::AdvanceDirectoryCookie {
                        count: core::num::NonZeroU64::new(1).unwrap(),
                    },
                    StateChange::BumpDirectoryGeneration(child_id),
                ],
                limits,
            )
            .await;

            let grant = ExportGrant::new(
                filesystem_id,
                root_id,
                owner,
                group,
                Vec::new(),
                1,
                1,
                false,
                false,
                1,
                CapabilitySet::ALL,
                engine_limits,
            )
            .unwrap();
            let policy = Policy(grant);
            let context = RequestContext::new(
                SessionId::new(12),
                PrincipalId::new("principal"),
                ExportId::new("export"),
            );
            let partial = execute_walk(
                &state,
                &policy,
                engine_limits,
                context.clone(),
                object_handle(root_id),
                vec![
                    ".".into(),
                    "child".into(),
                    ".".into(),
                    "nested".into(),
                    "missing".into(),
                ],
            )
            .await
            .unwrap();
            assert_eq!(partial.elements.len(), 4);
            assert_eq!(partial.elements[3].object, object_handle(nested_id));

            let clamped = execute_walk(
                &state,
                &policy,
                engine_limits,
                context.clone(),
                object_handle(root_id),
                vec!["..".into()],
            )
            .await
            .unwrap();
            assert_eq!(clamped.elements[0].object, object_handle(root_id));

            assert!(matches!(
                execute_walk(
                    &state,
                    &policy,
                    engine_limits,
                    context,
                    object_handle(root_id),
                    vec!["missing".into()],
                )
                .await,
                Err(ReadOperationError::Client(error)) if error.errno == LinuxErrno::ENOENT
            ));
        });
    }

    async fn commit(
        state: &w9pt_fs_state::testing::MemoryStateStore,
        filesystem_id: FilesystemId,
        fence: WriterFence,
        mutation_value: u128,
        changes: Vec<StateChange>,
        limits: StateLimits,
    ) {
        let request = CommitRequest::new(
            filesystem_id,
            MutationContext::new(
                MutationId::from_u128(mutation_value),
                RequestFingerprint::blake3(&mutation_value.to_be_bytes()),
                ClientIncarnationId::from_u128(100),
                MutationRetention::new(100),
            ),
            fence,
            if mutation_value == 9 {
                vec![
                    Precondition::RecordAbsent(RecordKey::Filesystem(filesystem_id)),
                    Precondition::RecordAbsent(RecordKey::Inode(
                        filesystem_id,
                        InodeId::from_u128(2),
                    )),
                ]
            } else {
                Vec::new()
            },
            changes,
            crate::encode_mutation_result(
                &w9pt::filesystem::FilesystemResult::Released,
                EngineLimits::default(),
                limits,
            )
            .unwrap(),
            limits,
        )
        .unwrap();
        assert!(matches!(
            state.commit(request).await.unwrap(),
            CommitOutcome::Committed(_)
        ));
    }
}
