//! Atomic named unlink, orphan creation, and inode retirement.

use w9pt::{
    LinuxErrno,
    filesystem::{FilesystemOperation, FilesystemRequest, FilesystemResult},
    protocol::UnlinkFlags,
};
use w9pt_fs_state::{
    CommitRequest, DirectoryCookie, EntryName, FilesystemStateStore, InodeData, InodeKind,
    InodeRecord, MutationContext, OrphanRecord, Precondition, ReadBatch, ReadConsistency,
    ReadOutcome, ReadQuery, ReadResult, RecordKey, RecordRevision, ScanBounds, StateChange,
    StateRecord,
};
use w9pt_fs_storage::TargetStore;

use crate::{
    EngineLimits, ExecutionContext, ExportGrant, ExportPolicy, ExportPolicyRequest, IdentitySource,
    authorization_client_error, check_directory_mutation, check_sticky_directory,
    encode_mutation_result, inode_id_from_handle, mutation_fingerprint, run_mutation,
};

use super::mutation::{
    MutationOperationError, MutationPlanError, client, fingerprint_error, runner_error,
};

pub(crate) async fn execute_unlink<S, T, P, I>(
    state: &S,
    policy: &P,
    _identities: &I,
    limits: EngineLimits,
    request: FilesystemRequest,
    execution: ExecutionContext,
) -> Result<FilesystemResult, MutationOperationError<S::Error, T::Error, P::Error, I::Error>>
where
    S: FilesystemStateStore,
    T: TargetStore,
    P: ExportPolicy,
    I: IdentitySource,
{
    let (directory, name, flags) = match &request.operation {
        FilesystemOperation::UnlinkAt {
            directory,
            name,
            flags,
        } => (*directory, name.clone(), *flags),
        _ => return Err(MutationOperationError::Internal),
    };
    if flags.bits() & !UnlinkFlags::REMOVE_DIR.bits() != 0 {
        return Err(MutationOperationError::Client(w9pt::FilesystemError::new(
            LinuxErrno::EINVAL,
        )));
    }
    let initial_grant = policy
        .resolve(ExportPolicyRequest::new(request.context.clone()))
        .await
        .map_err(MutationOperationError::Policy)?;
    let fingerprint = mutation_fingerprint(&request, &execution, &initial_grant, limits)
        .map_err(fingerprint_error)?;
    let mutation_id = execution
        .mutation_id
        .ok_or(MutationOperationError::Context(
            crate::ExecutionContextError::MissingMutationId,
        ))?;
    let fence = execution.fence.ok_or(MutationOperationError::Context(
        crate::ExecutionContextError::MissingWriterFence,
    ))?;
    let now = execution.timestamp.ok_or(MutationOperationError::Context(
        crate::ExecutionContextError::MissingTimestamp,
    ))?;
    let mutation = MutationContext::new(
        mutation_id,
        fingerprint,
        execution.client_incarnation,
        execution.retention,
    );
    let filesystem_id = initial_grant.filesystem_id();
    let root_inode_id = initial_grant.root_inode_id();
    let context = request.context.clone();
    run_mutation(
        state,
        filesystem_id,
        mutation,
        fence,
        w9pt::filesystem::FilesystemResultKind::Unlinked,
        limits,
        |_| {
            let context = context.clone();
            let name = name.clone();
            async move {
                let grant = policy
                    .resolve(ExportPolicyRequest::new(context))
                    .await
                    .map_err(MutationPlanError::Policy)?;
                if grant.filesystem_id() != filesystem_id || grant.root_inode_id() != root_inode_id
                {
                    return Err(client(LinuxErrno::EAGAIN));
                }
                plan_unlink::<S, T::Error, P::Error, I::Error>(
                    state, &grant, mutation, fence, now, directory, name, flags, limits,
                )
                .await
            }
        },
    )
    .await
    .map_err(runner_error)
}

#[allow(clippy::too_many_arguments)]
async fn plan_unlink<S, T, P, I>(
    state: &S,
    grant: &ExportGrant,
    mutation: MutationContext,
    fence: w9pt_fs_state::WriterFence,
    now: w9pt_fs_state::UnixTimestamp,
    directory: w9pt::filesystem::ObjectHandle,
    name: String,
    flags: UnlinkFlags,
    limits: EngineLimits,
) -> Result<CommitRequest, MutationPlanError<S::Error, T, P, I>>
where
    S: FilesystemStateStore,
{
    let state_limits = state.contract().limits();
    if name.len() > state_limits.max_entry_name_bytes() {
        return Err(client(LinuxErrno::ENAMETOOLONG));
    }
    let name =
        EntryName::new(name.into_bytes(), state_limits).map_err(|_| client(LinuxErrno::EINVAL))?;
    let parent_id = inode_id_from_handle(directory);
    let first = ReadBatch::new(
        grant.filesystem_id(),
        ReadConsistency::LatestLinearizable,
        vec![
            ReadQuery::Filesystem,
            ReadQuery::Inode(parent_id),
            ReadQuery::DirectoryEntry {
                parent_inode_id: parent_id,
                name: name.clone(),
            },
        ],
        state_limits,
    )
    .map_err(MutationPlanError::MalformedRead)?;
    let ReadOutcome::Snapshot(first) = state.read(first).await.map_err(MutationPlanError::State)?
    else {
        return Err(MutationPlanError::MalformedState);
    };
    validate_filesystem(&first.results()[0], grant)?;
    let entry = directory_entry(&first.results()[2])?.ok_or_else(|| client(LinuxErrno::ENOENT))?;
    let target_id = entry.child_inode_id();
    let mut queries = vec![
        ReadQuery::Filesystem,
        ReadQuery::Inode(parent_id),
        ReadQuery::DirectoryEntry {
            parent_inode_id: parent_id,
            name: name.clone(),
        },
        ReadQuery::Inode(target_id),
        ReadQuery::OpenPinCount(target_id),
        ReadQuery::Orphan(target_id),
    ];
    if flags.contains(UnlinkFlags::REMOVE_DIR) {
        queries.push(ReadQuery::DirectoryPage {
            parent_inode_id: target_id,
            after: DirectoryCookie::START,
            bounds: ScanBounds::new(1, state_limits.max_scan_bytes(), state_limits)
                .map_err(|_| MutationPlanError::MalformedState)?,
        });
    }
    let read = ReadBatch::new(
        grant.filesystem_id(),
        ReadConsistency::AtLeast(first.revision()),
        queries,
        state_limits,
    )
    .map_err(MutationPlanError::MalformedRead)?;
    let ReadOutcome::Snapshot(snapshot) =
        state.read(read).await.map_err(MutationPlanError::State)?
    else {
        return Err(MutationPlanError::MalformedState);
    };
    validate_filesystem(&snapshot.results()[0], grant)?;
    let parent = inode_record(&snapshot.results()[1])?.ok_or_else(|| client(LinuxErrno::EBADF))?;
    let current_entry =
        directory_entry(&snapshot.results()[2])?.ok_or_else(|| client(LinuxErrno::ENOENT))?;
    if current_entry.child_inode_id() != target_id {
        return Err(client(LinuxErrno::EAGAIN));
    }
    let target = inode_record(&snapshot.results()[3])?.ok_or(MutationPlanError::MalformedState)?;
    check_directory_mutation(grant, &parent)
        .and_then(|()| check_sticky_directory(grant, &parent, &target))
        .map_err(authorization_client_error)
        .map_err(MutationPlanError::Client)?;
    let removing_directory = flags.contains(UnlinkFlags::REMOVE_DIR);
    if removing_directory != (target.kind() == InodeKind::Directory) {
        return Err(client(if removing_directory {
            LinuxErrno::ENOTDIR
        } else {
            LinuxErrno::EISDIR
        }));
    }
    let ReadResult::OpenPinCount {
        inode_id: counted,
        count,
    } = &snapshot.results()[4]
    else {
        return Err(MutationPlanError::MalformedState);
    };
    if *counted != target_id {
        return Err(MutationPlanError::MalformedState);
    }
    let orphan = orphan_record(&snapshot.results()[5])?;
    if orphan.is_some() || target.link_count() == 0 {
        return Err(MutationPlanError::MalformedState);
    }
    if removing_directory {
        let ReadResult::DirectoryPage(page) = &snapshot.results()[6] else {
            return Err(MutationPlanError::MalformedState);
        };
        if !page.entries().is_empty() {
            return Err(client(LinuxErrno::ENOTEMPTY));
        }
        if *count != 0 {
            return Err(client(LinuxErrno::EBUSY));
        }
    }
    let fs = grant.filesystem_id();
    let parent_replacement = updated_parent(&parent, now)?;
    let new_links = target
        .link_count()
        .checked_sub(1)
        .ok_or_else(|| client(LinuxErrno::EOVERFLOW))?;
    let mut changes = vec![
        StateChange::Delete(RecordKey::DirectoryEntry(fs, parent_id, name.clone())),
        StateChange::Replace {
            key: RecordKey::Inode(fs, parent_id),
            record: StateRecord::Inode(parent_replacement),
        },
    ];
    if new_links > 0 {
        changes.push(StateChange::Replace {
            key: RecordKey::Inode(fs, target_id),
            record: StateRecord::Inode(updated_target(&target, new_links, now)?),
        });
    } else if *count > 0 {
        changes.push(StateChange::Replace {
            key: RecordKey::Inode(fs, target_id),
            record: StateRecord::Inode(updated_target(&target, 0, now)?),
        });
        changes.push(StateChange::Insert {
            key: RecordKey::Orphan(fs, target_id),
            record: StateRecord::Orphan(
                OrphanRecord::new(
                    target_id,
                    *count,
                    snapshot.revision(),
                    RecordRevision::new(1).expect("one is a valid placeholder revision"),
                )
                .map_err(|_| MutationPlanError::MalformedState)?,
            ),
        });
    } else {
        changes.push(StateChange::Delete(RecordKey::Inode(fs, target_id)));
    }
    let terminal = encode_mutation_result(&FilesystemResult::Unlinked, limits, state_limits)
        .map_err(MutationPlanError::ResultCodec)?;
    CommitRequest::new(
        fs,
        mutation,
        fence,
        vec![
            Precondition::FilesystemPolicyGeneration {
                expected: grant.policy_generation(),
            },
            Precondition::RecordRevision {
                key: RecordKey::Inode(fs, parent_id),
                expected: parent.revision(),
            },
            Precondition::DirectoryGeneration {
                inode_id: parent_id,
                expected: parent
                    .directory_generation()
                    .ok_or(MutationPlanError::MalformedState)?,
            },
            Precondition::RecordRevision {
                key: RecordKey::DirectoryEntry(fs, parent_id, name),
                expected: current_entry.revision(),
            },
            Precondition::RecordRevision {
                key: RecordKey::Inode(fs, target_id),
                expected: target.revision(),
            },
            Precondition::LinkCount {
                inode_id: target_id,
                expected: target.link_count(),
            },
            Precondition::OpenPinCount {
                inode_id: target_id,
                expected: *count,
            },
            Precondition::RecordAbsent(RecordKey::Orphan(fs, target_id)),
        ],
        changes,
        terminal,
        state_limits,
    )
    .map_err(MutationPlanError::MalformedCommit)
}

fn validate_filesystem<S, T, P, I>(
    result: &ReadResult,
    grant: &ExportGrant,
) -> Result<(), MutationPlanError<S, T, P, I>> {
    let ReadResult::Point {
        record: Some(record),
        ..
    } = result
    else {
        return Err(MutationPlanError::MalformedState);
    };
    let StateRecord::Filesystem(filesystem) = record.as_ref() else {
        return Err(MutationPlanError::MalformedState);
    };
    if filesystem.filesystem_id() != grant.filesystem_id()
        || filesystem.root_inode_id() != grant.root_inode_id()
        || filesystem.policy_generation() != grant.policy_generation()
    {
        return Err(client(LinuxErrno::EAGAIN));
    }
    Ok(())
}

fn inode_record<S, T, P, I>(
    result: &ReadResult,
) -> Result<Option<InodeRecord>, MutationPlanError<S, T, P, I>> {
    point(result, |record| match record {
        StateRecord::Inode(value) => Some(value.clone()),
        _ => None,
    })
}
fn directory_entry<S, T, P, I>(
    result: &ReadResult,
) -> Result<Option<w9pt_fs_state::DirectoryEntryRecord>, MutationPlanError<S, T, P, I>> {
    point(result, |record| match record {
        StateRecord::DirectoryEntry(value) => Some(value.clone()),
        _ => None,
    })
}
fn orphan_record<S, T, P, I>(
    result: &ReadResult,
) -> Result<Option<OrphanRecord>, MutationPlanError<S, T, P, I>> {
    point(result, |record| match record {
        StateRecord::Orphan(value) => Some(value.clone()),
        _ => None,
    })
}
fn point<S, T, P, I, R>(
    result: &ReadResult,
    convert: impl FnOnce(&StateRecord) -> Option<R>,
) -> Result<Option<R>, MutationPlanError<S, T, P, I>> {
    let ReadResult::Point { record, .. } = result else {
        return Err(MutationPlanError::MalformedState);
    };
    match record.as_deref() {
        Some(record) => convert(record)
            .map(Some)
            .ok_or(MutationPlanError::MalformedState),
        None => Ok(None),
    }
}

fn updated_parent<S, T, P, I>(
    inode: &InodeRecord,
    now: w9pt_fs_state::UnixTimestamp,
) -> Result<InodeRecord, MutationPlanError<S, T, P, I>> {
    let generation = inode
        .directory_generation()
        .ok_or(MutationPlanError::MalformedState)?
        .checked_next()
        .map_err(|_| client(LinuxErrno::EOVERFLOW))?;
    let parent = inode
        .directory_parent()
        .ok_or(MutationPlanError::MalformedState)?;
    rebuild(
        inode,
        inode.link_count(),
        now,
        InodeData::Directory {
            generation,
            parent_inode_id: parent,
        },
    )
}
fn updated_target<S, T, P, I>(
    inode: &InodeRecord,
    links: u64,
    now: w9pt_fs_state::UnixTimestamp,
) -> Result<InodeRecord, MutationPlanError<S, T, P, I>> {
    rebuild(inode, links, now, inode.data().clone())
}

fn rebuild<S, T, P, I>(
    inode: &InodeRecord,
    links: u64,
    now: w9pt_fs_state::UnixTimestamp,
    data: InodeData,
) -> Result<InodeRecord, MutationPlanError<S, T, P, I>> {
    let mut times = inode.times();
    times.changed = now;
    if matches!(data, InodeData::Directory { .. }) {
        times.modified = now;
    }
    let generation = inode
        .inode_generation()
        .checked_next()
        .map_err(|_| client(LinuxErrno::EOVERFLOW))?;
    let revision = RecordRevision::new(1).expect("one is a valid placeholder revision");
    match data {
        regular @ InodeData::RegularFile { .. } => InodeRecord::new_regular(
            inode.inode_id(),
            inode.qid_path(),
            revision,
            inode.mode(),
            inode.owner().clone(),
            inode.group().clone(),
            times,
            inode.logical_size(),
            links,
            generation,
            inode
                .content_context_id()
                .ok_or(MutationPlanError::MalformedState)?,
            regular,
        ),
        other => InodeRecord::new(
            inode.inode_id(),
            inode.qid_path(),
            revision,
            inode.mode(),
            inode.owner().clone(),
            inode.group().clone(),
            times,
            inode.logical_size(),
            links,
            generation,
            other,
        ),
    }
    .map_err(|_| MutationPlanError::MalformedState)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        operations::{execute_link, execute_release},
        testing::TestEnvironment,
    };
    use w9pt_fs_storage::{StorageMethod, testing::block_on};

    #[test]
    fn unlink_handles_links_orphans_and_final_release() {
        block_on(async {
            let environment = TestEnvironment::new(false, StorageMethod::Raw).await;
            let created = environment.create_file("original", 100).await;
            let link = FilesystemRequest::new(
                environment.context(),
                FilesystemOperation::Link {
                    directory: crate::object_handle(environment.root_id),
                    target: created.object,
                    name: "alias".into(),
                },
            );
            execute_link::<_, w9pt_fs_storage::testing::MemoryTarget, _, _>(
                &environment.state,
                &environment.policy,
                &environment.identities,
                environment.engine_limits,
                link,
                environment.execution(101),
            )
            .await
            .unwrap();
            for (mutation, name) in [(102, "original"), (103, "alias")] {
                let unlink = FilesystemRequest::new(
                    environment.context(),
                    FilesystemOperation::UnlinkAt {
                        directory: crate::object_handle(environment.root_id),
                        name: name.into(),
                        flags: UnlinkFlags::EMPTY,
                    },
                );
                assert_eq!(
                    execute_unlink::<_, w9pt_fs_storage::testing::MemoryTarget, _, _>(
                        &environment.state,
                        &environment.policy,
                        &environment.identities,
                        environment.engine_limits,
                        unlink,
                        environment.execution(mutation),
                    )
                    .await
                    .unwrap(),
                    FilesystemResult::Unlinked
                );
            }
            let inode_id = inode_id_from_handle(created.object);
            let read = ReadBatch::new(
                environment.filesystem_id,
                ReadConsistency::LatestLinearizable,
                vec![ReadQuery::Inode(inode_id), ReadQuery::Orphan(inode_id)],
                environment.state_limits,
            )
            .unwrap();
            let ReadOutcome::Snapshot(snapshot) = environment.state.read(read).await.unwrap()
            else {
                panic!("orphan unavailable")
            };
            assert!(
                matches!(&snapshot.results()[0], ReadResult::Point { record: Some(record), .. }
                if matches!(record.as_ref(), StateRecord::Inode(inode) if inode.link_count() == 0))
            );
            assert!(matches!(
                &snapshot.results()[1],
                ReadResult::Point {
                    record: Some(_),
                    ..
                }
            ));
            let release = FilesystemRequest::new(
                environment.context(),
                FilesystemOperation::Release {
                    object: created.object,
                    open: Some(created.open),
                    xattr: None,
                },
            );
            execute_release::<_, w9pt_fs_storage::testing::MemoryTarget, _, _>(
                &environment.state,
                &environment.policy,
                &environment.identities,
                environment.engine_limits,
                release,
                environment.execution(104),
            )
            .await
            .unwrap();
            let read = ReadBatch::new(
                environment.filesystem_id,
                ReadConsistency::LatestLinearizable,
                vec![ReadQuery::Inode(inode_id)],
                environment.state_limits,
            )
            .unwrap();
            let ReadOutcome::Snapshot(snapshot) = environment.state.read(read).await.unwrap()
            else {
                panic!("retirement unavailable")
            };
            assert!(matches!(
                &snapshot.results()[0],
                ReadResult::Point { record: None, .. }
            ));
        });
    }
}
