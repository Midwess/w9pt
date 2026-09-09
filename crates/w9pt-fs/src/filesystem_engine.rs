//! Runtime-neutral semantic-engine composition and attach resolution.

use core::fmt;

use w9pt::filesystem::{AttachResult, FilesystemOperation, FilesystemResult, RequestContext};
use w9pt::{FilesystemError, LinuxErrno};
use w9pt_fs_state::{
    FilesystemStateStore, InodeKind, ReadBatch, ReadConsistency, ReadOutcome, ReadQuery,
    ReadResult, StateLimitError, StateRecord,
};
use w9pt_fs_storage::{ContentRepository, StorageError, TargetStore};

use crate::{
    EngineLimits, EngineOutcome, EngineProviderFailure, EngineRequest, EngineTerminal,
    ExportPolicy, ExportPolicyRequest, IdentitySource, UnresolvedEngineFailure,
    derive_capabilities, object_handle,
    operations::{
        DataReadError, MutationOperationError, ReadOperationError, execute_create, execute_fsync,
        execute_getattr, execute_link, execute_mkdir, execute_open, execute_read, execute_readdir,
        execute_readlink, execute_release, execute_rename, execute_setattr, execute_symlink,
        execute_unlink, execute_walk, execute_write,
    },
    qid_from_inode,
};

/// Stateless semantic coordinator over caller-owned state, content, policy, and identities.
pub struct FilesystemEngine<S, T, P, I> {
    pub(crate) state: S,
    pub(crate) content: ContentRepository<T>,
    pub(crate) policy: P,
    pub(crate) identities: I,
    pub(crate) limits: EngineLimits,
}

impl<S, T, P, I> FilesystemEngine<S, T, P, I> {
    /// Composes an engine without opening clients, reading clocks, or starting a runtime.
    pub const fn new(
        state: S,
        content: ContentRepository<T>,
        policy: P,
        identities: I,
        limits: EngineLimits,
    ) -> Self {
        Self {
            state,
            content,
            policy,
            identities,
            limits,
        }
    }

    /// Returns the validated semantic limits.
    pub const fn limits(&self) -> EngineLimits {
        self.limits
    }

    /// Returns the caller-owned state client.
    pub const fn state(&self) -> &S {
        &self.state
    }

    /// Returns the immutable content repository.
    pub const fn content(&self) -> &ContentRepository<T> {
        &self.content
    }

    /// Returns the caller-owned deterministic identity source.
    pub const fn identities(&self) -> &I {
        &self.identities
    }
}

impl<S, T, P, I> FilesystemEngine<S, T, P, I>
where
    S: FilesystemStateStore,
    T: TargetStore,
    P: ExportPolicy,
{
    /// Resolves an authenticated attach context into its stable root and honest capabilities.
    pub async fn resolve_attach(
        &self,
        context: RequestContext,
    ) -> Result<AttachResult, AttachResolutionError<S::Error, P::Error>> {
        let principal = context.principal.clone();
        let export = context.export.clone();
        let grant = self
            .policy
            .resolve(ExportPolicyRequest::new(context))
            .await
            .map_err(AttachResolutionError::Policy)?;
        let state_limits = self.state.contract().limits();
        let request = ReadBatch::new(
            grant.filesystem_id(),
            ReadConsistency::LatestLinearizable,
            vec![
                ReadQuery::Filesystem,
                ReadQuery::Inode(grant.root_inode_id()),
            ],
            state_limits,
        )
        .map_err(AttachResolutionError::MalformedRead)?;
        let outcome = self
            .state
            .read(request)
            .await
            .map_err(AttachResolutionError::State)?;
        let ReadOutcome::Snapshot(snapshot) = outcome else {
            return Err(AttachResolutionError::Unavailable);
        };
        let [filesystem_result, root_result] = snapshot.results() else {
            return Err(AttachResolutionError::MalformedState);
        };
        let ReadResult::Point {
            record: Some(filesystem),
            ..
        } = filesystem_result
        else {
            return Err(AttachResolutionError::MissingFilesystem);
        };
        let StateRecord::Filesystem(filesystem) = filesystem.as_ref() else {
            return Err(AttachResolutionError::MalformedState);
        };
        let ReadResult::Point {
            record: Some(root), ..
        } = root_result
        else {
            return Err(AttachResolutionError::MissingRoot);
        };
        let StateRecord::Inode(root) = root.as_ref() else {
            return Err(AttachResolutionError::MalformedState);
        };
        if filesystem.filesystem_id() != grant.filesystem_id()
            || filesystem.root_inode_id() != grant.root_inode_id()
            || filesystem.policy_generation() != grant.policy_generation()
            || root.inode_id() != grant.root_inode_id()
            || root.kind() != InodeKind::Directory
            || root.directory_parent() != Some(root.inode_id())
        {
            return Err(AttachResolutionError::GrantMismatch);
        }
        Ok(AttachResult {
            principal,
            export,
            root: object_handle(root.inode_id()),
            qid: qid_from_inode(root),
            capabilities: derive_capabilities(
                self.state.contract(),
                self.content.target().guarantees(),
                grant.capability_ceiling(),
                grant.read_only(),
            ),
        })
    }
}

impl<S, T, P, I> FilesystemEngine<S, T, P, I>
where
    S: FilesystemStateStore,
    T: TargetStore,
    P: ExportPolicy,
    I: IdentitySource,
{
    /// Executes one owned filesystem effect and preserves its operation identity.
    pub async fn execute(
        &self,
        request: EngineRequest,
    ) -> EngineOutcome<S::Error, StorageError<T::Error>, EngineProviderFailure<P::Error, I::Error>>
    {
        let operation_id = request.operation_id;
        if self
            .limits
            .check_retained_bytes(estimated_request_bytes(&request.request))
            .is_err()
        {
            return terminal_error(operation_id, LinuxErrno::ENOMEM);
        }
        let complete_request = request.request.clone();
        let execution = request.execution;
        let result = match request.request.operation {
            FilesystemOperation::Walk { start, names } => execute_walk(
                &self.state,
                &self.policy,
                self.limits,
                request.request.context,
                start,
                names,
            )
            .await
            .map(FilesystemResult::Walked),
            FilesystemOperation::Open { .. } => {
                return mutation_outcome(
                    operation_id,
                    execute_open::<S, T, P, I>(
                        &self.state,
                        &self.content,
                        &self.policy,
                        &self.identities,
                        self.limits,
                        complete_request,
                        execution,
                    )
                    .await,
                );
            }
            FilesystemOperation::Create { .. } => {
                return mutation_outcome(
                    operation_id,
                    execute_create(
                        &self.state,
                        &self.content,
                        &self.policy,
                        &self.identities,
                        self.limits,
                        complete_request,
                        execution,
                    )
                    .await,
                );
            }
            FilesystemOperation::Mkdir { .. } => {
                return mutation_outcome(
                    operation_id,
                    execute_mkdir::<S, T, P, I>(
                        &self.state,
                        &self.policy,
                        &self.identities,
                        self.limits,
                        complete_request,
                        execution,
                    )
                    .await,
                );
            }
            FilesystemOperation::Symlink { .. } => {
                return mutation_outcome(
                    operation_id,
                    execute_symlink::<S, T, P, I>(
                        &self.state,
                        &self.policy,
                        &self.identities,
                        self.limits,
                        complete_request,
                        execution,
                    )
                    .await,
                );
            }
            FilesystemOperation::Link { .. } => {
                return mutation_outcome(
                    operation_id,
                    execute_link::<S, T, P, I>(
                        &self.state,
                        &self.policy,
                        &self.identities,
                        self.limits,
                        complete_request,
                        execution,
                    )
                    .await,
                );
            }
            FilesystemOperation::UnlinkAt { .. } => {
                return mutation_outcome(
                    operation_id,
                    execute_unlink::<S, T, P, I>(
                        &self.state,
                        &self.policy,
                        &self.identities,
                        self.limits,
                        complete_request,
                        execution,
                    )
                    .await,
                );
            }
            FilesystemOperation::RenameAt { .. } => {
                return mutation_outcome(
                    operation_id,
                    execute_rename::<S, T, P, I>(
                        &self.state,
                        &self.policy,
                        &self.identities,
                        self.limits,
                        complete_request,
                        execution,
                    )
                    .await,
                );
            }
            FilesystemOperation::Read {
                open,
                offset,
                count,
            } => {
                return read_outcome(
                    operation_id,
                    execute_read(
                        &self.state,
                        &self.content,
                        &self.policy,
                        self.limits,
                        request.request.context,
                        execution.client_incarnation,
                        open,
                        offset,
                        count,
                    )
                    .await,
                );
            }
            FilesystemOperation::Write { .. } => {
                return mutation_outcome(
                    operation_id,
                    execute_write(
                        &self.state,
                        &self.content,
                        &self.policy,
                        &self.identities,
                        self.limits,
                        complete_request,
                        execution,
                    )
                    .await,
                );
            }
            FilesystemOperation::Setattr { .. } => {
                return mutation_outcome(
                    operation_id,
                    execute_setattr(
                        &self.state,
                        &self.content,
                        &self.policy,
                        &self.identities,
                        self.limits,
                        complete_request,
                        execution,
                    )
                    .await,
                );
            }
            FilesystemOperation::Fsync { open, data_only } => {
                return sync_outcome(
                    operation_id,
                    execute_fsync(
                        &self.state,
                        &self.content,
                        &self.policy,
                        self.limits,
                        request.request.context,
                        execution.client_incarnation,
                        open,
                        data_only,
                    )
                    .await,
                );
            }
            FilesystemOperation::Release { .. } => {
                return mutation_outcome(
                    operation_id,
                    execute_release::<S, T, P, I>(
                        &self.state,
                        &self.policy,
                        &self.identities,
                        self.limits,
                        complete_request,
                        execution,
                    )
                    .await,
                );
            }
            FilesystemOperation::Getattr { object, mask } => execute_getattr(
                &self.state,
                &self.policy,
                self.limits,
                request.request.context,
                object,
                mask,
            )
            .await
            .map(FilesystemResult::Attributes),
            FilesystemOperation::Readlink { object } => execute_readlink(
                &self.state,
                &self.policy,
                self.limits,
                request.request.context,
                object,
            )
            .await
            .map(FilesystemResult::LinkTarget),
            FilesystemOperation::ReadDir {
                open,
                offset,
                count,
            } => execute_readdir(
                &self.state,
                &self.policy,
                self.limits,
                request.request.context,
                execution.client_incarnation,
                open,
                offset,
                count,
            )
            .await
            .map(FilesystemResult::DirectoryRead),
            _ => return terminal_error(operation_id, LinuxErrno::EOPNOTSUPP),
        };
        match result {
            Ok(result) => EngineOutcome::Terminal(EngineTerminal {
                operation_id,
                result: Ok(result),
            }),
            Err(ReadOperationError::Client(error)) => EngineOutcome::Terminal(EngineTerminal {
                operation_id,
                result: Err(error),
            }),
            Err(ReadOperationError::State(error)) => {
                EngineOutcome::Unresolved(UnresolvedEngineFailure {
                    operation_id,
                    failure: crate::ExecutionFailure::State(error),
                })
            }
            Err(ReadOperationError::Policy(error)) => {
                EngineOutcome::Unresolved(UnresolvedEngineFailure {
                    operation_id,
                    failure: crate::ExecutionFailure::Policy(EngineProviderFailure::Policy(error)),
                })
            }
            Err(
                ReadOperationError::MalformedRead(_)
                | ReadOperationError::RevisionUnavailable
                | ReadOperationError::MalformedState,
            ) => terminal_error(operation_id, LinuxErrno::EIO),
        }
    }
}

fn mutation_outcome<S, T, P, I>(
    operation_id: w9pt::OperationId,
    result: Result<FilesystemResult, MutationOperationError<S, T, P, I>>,
) -> EngineOutcome<S, StorageError<T>, EngineProviderFailure<P, I>> {
    match result {
        Ok(result) => EngineOutcome::Terminal(EngineTerminal {
            operation_id,
            result: Ok(result),
        }),
        Err(MutationOperationError::Client(error)) => EngineOutcome::Terminal(EngineTerminal {
            operation_id,
            result: Err(error),
        }),
        Err(MutationOperationError::State(error)) => {
            EngineOutcome::Unresolved(UnresolvedEngineFailure {
                operation_id,
                failure: crate::ExecutionFailure::State(error),
            })
        }
        Err(MutationOperationError::Target(error)) => match error {
            StorageError::Target(_) | StorageError::Ambiguous(_) => {
                EngineOutcome::Unresolved(UnresolvedEngineFailure {
                    operation_id,
                    failure: crate::ExecutionFailure::Target(error),
                })
            }
            StorageError::Range(_) => terminal_error(operation_id, LinuxErrno::EOVERFLOW),
            StorageError::Limit(_) => terminal_error(operation_id, LinuxErrno::ENOMEM),
            StorageError::Conflict(_) => terminal_error(operation_id, LinuxErrno::EAGAIN),
            StorageError::Configuration(_) | StorageError::Representation(_) => {
                terminal_error(operation_id, LinuxErrno::EOPNOTSUPP)
            }
            StorageError::Format(_)
            | StorageError::Corruption(_)
            | StorageError::Preparation(_)
            | StorageError::Missing { .. } => terminal_error(operation_id, LinuxErrno::EIO),
        },
        Err(MutationOperationError::Policy(error)) => {
            EngineOutcome::Unresolved(UnresolvedEngineFailure {
                operation_id,
                failure: crate::ExecutionFailure::Policy(EngineProviderFailure::Policy(error)),
            })
        }
        Err(MutationOperationError::Identity(error)) => {
            EngineOutcome::Unresolved(UnresolvedEngineFailure {
                operation_id,
                failure: crate::ExecutionFailure::Policy(EngineProviderFailure::Identity(error)),
            })
        }
        Err(MutationOperationError::Context(error)) => {
            EngineOutcome::Unresolved(UnresolvedEngineFailure {
                operation_id,
                failure: crate::ExecutionFailure::Context(error),
            })
        }
        Err(MutationOperationError::Authority(error)) => {
            EngineOutcome::Unresolved(UnresolvedEngineFailure {
                operation_id,
                failure: crate::ExecutionFailure::Authority(error),
            })
        }
        Err(MutationOperationError::Internal) => terminal_error(operation_id, LinuxErrno::EIO),
    }
}

fn read_outcome<S, T, P, I>(
    operation_id: w9pt::OperationId,
    result: Result<Vec<u8>, DataReadError<S, T, P>>,
) -> EngineOutcome<S, StorageError<T>, EngineProviderFailure<P, I>> {
    match result {
        Ok(bytes) => EngineOutcome::Terminal(EngineTerminal {
            operation_id,
            result: Ok(FilesystemResult::Read(bytes)),
        }),
        Err(DataReadError::Client(error)) => EngineOutcome::Terminal(EngineTerminal {
            operation_id,
            result: Err(error),
        }),
        Err(DataReadError::State(error)) => EngineOutcome::Unresolved(UnresolvedEngineFailure {
            operation_id,
            failure: crate::ExecutionFailure::State(error),
        }),
        Err(DataReadError::Target(error)) => match error {
            StorageError::Target(_) | StorageError::Ambiguous(_) => {
                EngineOutcome::Unresolved(UnresolvedEngineFailure {
                    operation_id,
                    failure: crate::ExecutionFailure::Target(error),
                })
            }
            StorageError::Range(_) => terminal_error(operation_id, LinuxErrno::EOVERFLOW),
            StorageError::Limit(_) => terminal_error(operation_id, LinuxErrno::ENOMEM),
            StorageError::Conflict(_) => terminal_error(operation_id, LinuxErrno::EAGAIN),
            StorageError::Configuration(_) | StorageError::Representation(_) => {
                terminal_error(operation_id, LinuxErrno::EOPNOTSUPP)
            }
            StorageError::Format(_)
            | StorageError::Corruption(_)
            | StorageError::Preparation(_)
            | StorageError::Missing { .. } => terminal_error(operation_id, LinuxErrno::EIO),
        },
        Err(DataReadError::Policy(error)) => EngineOutcome::Unresolved(UnresolvedEngineFailure {
            operation_id,
            failure: crate::ExecutionFailure::Policy(EngineProviderFailure::Policy(error)),
        }),
        Err(DataReadError::MalformedRead(error)) => {
            let _ = error;
            terminal_error(operation_id, LinuxErrno::EIO)
        }
        Err(DataReadError::RevisionUnavailable | DataReadError::MalformedState) => {
            terminal_error(operation_id, LinuxErrno::EIO)
        }
    }
}

fn sync_outcome<S, T, P, I>(
    operation_id: w9pt::OperationId,
    result: Result<(), DataReadError<S, T, P>>,
) -> EngineOutcome<S, StorageError<T>, EngineProviderFailure<P, I>> {
    match result {
        Ok(()) => EngineOutcome::Terminal(EngineTerminal {
            operation_id,
            result: Ok(FilesystemResult::Synced),
        }),
        Err(DataReadError::Client(error)) => EngineOutcome::Terminal(EngineTerminal {
            operation_id,
            result: Err(error),
        }),
        Err(DataReadError::State(error)) => EngineOutcome::Unresolved(UnresolvedEngineFailure {
            operation_id,
            failure: crate::ExecutionFailure::State(error),
        }),
        Err(DataReadError::Target(error)) => match error {
            StorageError::Target(_) | StorageError::Ambiguous(_) => {
                EngineOutcome::Unresolved(UnresolvedEngineFailure {
                    operation_id,
                    failure: crate::ExecutionFailure::Target(error),
                })
            }
            StorageError::Range(_) => terminal_error(operation_id, LinuxErrno::EOVERFLOW),
            StorageError::Limit(_) => terminal_error(operation_id, LinuxErrno::ENOMEM),
            StorageError::Conflict(_) => terminal_error(operation_id, LinuxErrno::EAGAIN),
            StorageError::Configuration(_) | StorageError::Representation(_) => {
                terminal_error(operation_id, LinuxErrno::EOPNOTSUPP)
            }
            StorageError::Format(_)
            | StorageError::Corruption(_)
            | StorageError::Preparation(_)
            | StorageError::Missing { .. } => terminal_error(operation_id, LinuxErrno::EIO),
        },
        Err(DataReadError::Policy(error)) => EngineOutcome::Unresolved(UnresolvedEngineFailure {
            operation_id,
            failure: crate::ExecutionFailure::Policy(EngineProviderFailure::Policy(error)),
        }),
        Err(DataReadError::MalformedRead(error)) => {
            let _ = error;
            terminal_error(operation_id, LinuxErrno::EIO)
        }
        Err(DataReadError::RevisionUnavailable | DataReadError::MalformedState) => {
            terminal_error(operation_id, LinuxErrno::EIO)
        }
    }
}

fn estimated_request_bytes(request: &w9pt::filesystem::FilesystemRequest) -> usize {
    let operation = match &request.operation {
        FilesystemOperation::Walk { names, .. } => names.iter().map(String::len).sum(),
        FilesystemOperation::Write { data, .. } | FilesystemOperation::XattrWrite { data, .. } => {
            data.len()
        }
        FilesystemOperation::Create { name, .. }
        | FilesystemOperation::Mkdir { name, .. }
        | FilesystemOperation::Mknod { name, .. }
        | FilesystemOperation::Rename { name, .. }
        | FilesystemOperation::UnlinkAt { name, .. }
        | FilesystemOperation::Link { name, .. }
        | FilesystemOperation::XattrWalk { name, .. }
        | FilesystemOperation::XattrCreate { name, .. } => name.len(),
        FilesystemOperation::Symlink { name, target, .. } => {
            name.len().saturating_add(target.len())
        }
        FilesystemOperation::RenameAt {
            old_name, new_name, ..
        } => old_name.len().saturating_add(new_name.len()),
        _ => 0,
    };
    request
        .context
        .principal
        .as_str()
        .len()
        .saturating_add(request.context.export.as_str().len())
        .saturating_add(operation)
}

fn terminal_error<S, T, P>(
    operation_id: w9pt::OperationId,
    errno: LinuxErrno,
) -> EngineOutcome<S, T, P> {
    EngineOutcome::Terminal(EngineTerminal {
        operation_id,
        result: Err(FilesystemError::new(errno)),
    })
}

/// Failure resolving an attach policy into authoritative root state.
#[derive(Debug)]
pub enum AttachResolutionError<S, P> {
    /// Caller-owned export policy failed.
    Policy(P),
    /// Authoritative state adapter failed.
    State(S),
    /// The state request exceeded the adapter's advertised limits.
    MalformedRead(StateLimitError),
    /// A latest authoritative snapshot was not available.
    Unavailable,
    /// The selected filesystem has not been bootstrapped.
    MissingFilesystem,
    /// The selected export root is absent.
    MissingRoot,
    /// State returned a record family inconsistent with its query.
    MalformedState,
    /// Policy and authoritative filesystem/root identity disagree.
    GrantMismatch,
}

impl<S: fmt::Display, P: fmt::Display> fmt::Display for AttachResolutionError<S, P> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Policy(error) => write!(formatter, "attach policy failed: {error}"),
            Self::State(error) => write!(formatter, "attach state read failed: {error}"),
            Self::MalformedRead(error) => error.fmt(formatter),
            Self::Unavailable => formatter.write_str("attach state revision unavailable"),
            Self::MissingFilesystem => formatter.write_str("attach filesystem is missing"),
            Self::MissingRoot => formatter.write_str("attach root is missing"),
            Self::MalformedState => formatter.write_str("attach state result is malformed"),
            Self::GrantMismatch => formatter.write_str("attach grant disagrees with state"),
        }
    }
}

impl<S, P> std::error::Error for AttachResolutionError<S, P>
where
    S: std::error::Error + 'static,
    P: std::error::Error + 'static,
{
}

#[cfg(test)]
mod tests {
    use core::convert::Infallible;

    use super::*;
    use w9pt::{
        filesystem::{Capability, CapabilitySet, ExportId, PrincipalId},
        protocol::SessionId,
    };
    use w9pt_fs_state::{
        AcquireLeaseOutcome, AcquireWriterLease, ClientIncarnationId, CommitOutcome, CommitRequest,
        DirectoryCookie, DirectoryGeneration, FencingToken, FilesystemId, FilesystemRecord,
        GroupId, InodeData, InodeGeneration, InodeId, InodeRecord, InodeTimes, LeaseDuration,
        LeaseId, LeaseOperationId, ManualLeaseClock, MutationContext, MutationRetention,
        Precondition, PrincipalId as StatePrincipalId, QidPath, RecordKey, RecordRevision,
        RequestFingerprint, StateChange, StateLimits, StateRecord, StateRevision, WriterFence,
        WriterIncarnationId, WriterScopeId, WriterTopology, testing::MemoryAuthority,
    };
    use w9pt_fs_storage::{
        ContentRepository, CreationDefaults, MutationId, StorageLimits, StorageMethod,
        testing::{MemoryTarget, block_on},
    };

    use crate::{
        CanonicalIdentity, ExecutionContext, ExportGrant, IdentityMappingRequest, NumericIdentity,
        ReverseIdentityMappingRequest,
    };

    #[derive(Clone)]
    struct FixedPolicy(ExportGrant);

    impl ExportPolicy for FixedPolicy {
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
    fn attach_resolves_one_authoritative_self_parented_root() {
        block_on(async {
            let state_limits = StateLimits::default();
            let engine_limits = EngineLimits::default();
            let filesystem_id = FilesystemId::from_u128(1);
            let root_id = InodeId::from_u128(2);
            let authority = MemoryAuthority::new(
                WriterTopology::SerializableMultiWriter,
                state_limits,
                ManualLeaseClock::new(w9pt_fs_state::LeaseDeadline::new(0)),
            );
            let state = authority.open_client();
            let acquire = AcquireWriterLease::new(
                filesystem_id,
                LeaseOperationId::from_u128(3),
                WriterScopeId::from_u128(4),
                WriterIncarnationId::from_u128(5),
                LeaseId::from_u128(6),
                LeaseDuration::new(10).unwrap(),
                state_limits,
            )
            .unwrap();
            let AcquireLeaseOutcome::Granted(grant) =
                state.acquire_writer_lease(acquire).await.unwrap()
            else {
                panic!("lease was not granted")
            };
            let timestamp = w9pt_fs_state::UnixTimestamp::new(0, 0).unwrap();
            let root = InodeRecord::new(
                root_id,
                QidPath::new(1).unwrap(),
                RecordRevision::new(1).unwrap(),
                0o755,
                StatePrincipalId::new(b"owner".to_vec(), state_limits).unwrap(),
                GroupId::new(b"group".to_vec(), state_limits).unwrap(),
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
                    parent_inode_id: root_id,
                },
            )
            .unwrap();
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
            let mutation = MutationContext::new(
                MutationId::from_u128(7),
                RequestFingerprint::blake3(b"bootstrap"),
                ClientIncarnationId::from_u128(8),
                MutationRetention::new(100),
            );
            let request = CommitRequest::new(
                filesystem_id,
                mutation,
                grant.fence,
                vec![
                    Precondition::RecordAbsent(RecordKey::Filesystem(filesystem_id)),
                    Precondition::RecordAbsent(RecordKey::Inode(filesystem_id, root_id)),
                ],
                vec![
                    StateChange::Insert {
                        key: RecordKey::Filesystem(filesystem_id),
                        record: StateRecord::Filesystem(filesystem),
                    },
                    StateChange::Insert {
                        key: RecordKey::Inode(filesystem_id, root_id),
                        record: StateRecord::Inode(root),
                    },
                ],
                crate::encode_mutation_result(
                    &w9pt::filesystem::FilesystemResult::Released,
                    engine_limits,
                    state_limits,
                )
                .unwrap(),
                state_limits,
            )
            .unwrap();
            assert!(matches!(
                state.commit(request).await.unwrap(),
                CommitOutcome::Committed(_)
            ));

            let policy = FixedPolicy(
                ExportGrant::new(
                    filesystem_id,
                    root_id,
                    StatePrincipalId::new(b"owner".to_vec(), state_limits).unwrap(),
                    GroupId::new(b"group".to_vec(), state_limits).unwrap(),
                    Vec::new(),
                    1,
                    1,
                    false,
                    false,
                    1,
                    CapabilitySet::ALL,
                    engine_limits,
                )
                .unwrap(),
            );
            let repository = ContentRepository::new(
                MemoryTarget::new(),
                "attach-test",
                CreationDefaults::new(StorageMethod::BlockSplit),
                StorageLimits::default(),
            )
            .unwrap();
            let engine = FilesystemEngine::new(state, repository, policy, (), engine_limits);
            let attach = engine
                .resolve_attach(RequestContext::new(
                    SessionId::new(9),
                    PrincipalId::new("principal"),
                    ExportId::new("export"),
                ))
                .await
                .unwrap();
            assert_eq!(attach.root, object_handle(root_id));
            assert_eq!(attach.qid.path, 1);
            assert!(attach.capabilities.contains(Capability::Walk));
            assert!(!attach.capabilities.contains(Capability::Mknod));
            assert!(!attach.capabilities.contains(Capability::DurableMetadata));
        });
    }

    #[test]
    fn real_session_effects_attach_execute_and_reject_deferred_work() {
        use crate::testing::TestEnvironment;
        use w9pt::{
            Completion, Effect, PolicyResult, Session, SessionConfig, SessionContext,
            filesystem::{AttachResult, FilesystemResult},
            protocol::{GetattrMask, MessageType},
        };
        use w9pt_fs_storage::StorageMethod;

        block_on(async {
            let environment = TestEnvironment::new(false, StorageMethod::Raw).await;
            let engine = environment.engine();
            let mut session = Session::new(
                SessionConfig::default(),
                SessionContext::new(SessionId::new(10)),
            )
            .unwrap();
            negotiate(&mut session);
            let attach_operation = emit_attach(&mut session);
            let attach = engine.resolve_attach(environment.context()).await.unwrap();
            session
                .complete(Completion::Policy {
                    operation_id: attach_operation,
                    result: Ok(PolicyResult::Attached(attach)),
                })
                .unwrap();
            expect_response(&mut session, MessageType::Rattach);

            let mut getattr = Vec::new();
            getattr.extend_from_slice(&1u32.to_le_bytes());
            getattr.extend_from_slice(&GetattrMask::ALL.bits().to_le_bytes());
            session
                .receive_frame(frame(MessageType::Tgetattr, 3, &getattr))
                .unwrap();
            let Effect::Filesystem {
                operation_id,
                request,
            } = session.poll_effect().unwrap()
            else {
                panic!("expected getattr filesystem effect")
            };
            let outcome = engine
                .execute(EngineRequest::new(
                    operation_id,
                    request,
                    crate::ExecutionContext::read_only(
                        environment.client,
                        MutationRetention::new(1_000),
                    ),
                ))
                .await;
            let EngineOutcome::Terminal(terminal) = outcome else {
                panic!("getattr did not produce a terminal result")
            };
            assert!(matches!(
                terminal.result,
                Ok(FilesystemResult::Attributes(_))
            ));
            session.complete(terminal.into_completion()).unwrap();
            expect_response(&mut session, MessageType::Rgetattr);

            let mut deferred = Session::new(
                SessionConfig::default(),
                SessionContext::new(SessionId::new(10)),
            )
            .unwrap();
            negotiate(&mut deferred);
            let attach_operation = emit_attach(&mut deferred);
            deferred
                .complete(Completion::Policy {
                    operation_id: attach_operation,
                    result: Ok(PolicyResult::Attached(AttachResult {
                        principal: PrincipalId::new("principal"),
                        export: ExportId::new("export"),
                        root: object_handle(environment.root_id),
                        qid: w9pt::Qid::new(w9pt::protocol::QidType::DIRECTORY, 0, 1),
                        capabilities: CapabilitySet::ALL,
                    })),
                })
                .unwrap();
            expect_response(&mut deferred, MessageType::Rattach);
            let before_state = environment.authority.trace().unwrap().len();
            environment.target.clear_trace().unwrap();
            deferred
                .receive_frame(frame(MessageType::Tstatfs, 4, &1u32.to_le_bytes()))
                .unwrap();
            let Effect::Filesystem {
                operation_id,
                request,
            } = deferred.poll_effect().unwrap()
            else {
                panic!("expected statfs filesystem effect")
            };
            let outcome = engine
                .execute(EngineRequest::new(
                    operation_id,
                    request,
                    crate::ExecutionContext::read_only(
                        environment.client,
                        MutationRetention::new(1_000),
                    ),
                ))
                .await;
            let EngineOutcome::Terminal(terminal) = outcome else {
                panic!("deferred operation did not terminate")
            };
            assert!(matches!(
                terminal.result,
                Err(ref error) if error.errno == LinuxErrno::EOPNOTSUPP
            ));
            assert_eq!(environment.authority.trace().unwrap().len(), before_state);
            assert!(environment.target.trace().unwrap().is_empty());
            deferred.complete(terminal.into_completion()).unwrap();
            expect_response(&mut deferred, MessageType::Rlerror);
        });
    }

    #[test]
    fn real_session_mutations_replay_across_engines_after_handoff_and_response_loss() {
        use crate::testing::TestEnvironment;
        use w9pt::protocol::{MessageType, OpenFlags, SetattrMask};
        use w9pt::{Completion, PolicyResult, Session, SessionConfig, SessionContext};

        block_on(async {
            let environment = TestEnvironment::new(false, StorageMethod::Raw).await;
            let first_engine = environment.engine();
            let second_engine = FilesystemEngine::new(
                environment.authority.open_client(),
                ContentRepository::new(
                    environment.target.clone(),
                    "semantic-engine-tests",
                    CreationDefaults::new(StorageMethod::BlockSplit),
                    StorageLimits::default(),
                )
                .unwrap(),
                environment.policy.clone(),
                environment.identities,
                environment.engine_limits,
            );
            let mut session = Session::new(
                SessionConfig::default(),
                SessionContext::new(SessionId::new(10)),
            )
            .unwrap();
            negotiate(&mut session);
            let attach_operation = emit_attach(&mut session);
            let attach = first_engine
                .resolve_attach(environment.context())
                .await
                .unwrap();
            session
                .complete(Completion::Policy {
                    operation_id: attach_operation,
                    result: Ok(PolicyResult::Attached(attach)),
                })
                .unwrap();
            expect_response(&mut session, MessageType::Rattach);

            let mut clone_root = Vec::new();
            clone_root.extend_from_slice(&1u32.to_le_bytes());
            clone_root.extend_from_slice(&2u32.to_le_bytes());
            clone_root.extend_from_slice(&0u16.to_le_bytes());
            session
                .receive_frame(frame(MessageType::Twalk, 3, &clone_root))
                .unwrap();
            expect_response(&mut session, MessageType::Rwalk);

            let mut create = Vec::new();
            create.extend_from_slice(&2u32.to_le_bytes());
            create.extend_from_slice(&wire_string("file"));
            create.extend_from_slice(&OpenFlags::RDWR.bits().to_le_bytes());
            create.extend_from_slice(&0o660u32.to_le_bytes());
            create.extend_from_slice(&1u32.to_le_bytes());
            session
                .receive_frame(frame(MessageType::Tlcreate, 4, &create))
                .unwrap();
            complete_session_effect(
                &first_engine,
                &mut session,
                environment.execution(900),
                MessageType::Rlcreate,
            )
            .await;

            let mut write = Vec::new();
            write.extend_from_slice(&2u32.to_le_bytes());
            write.extend_from_slice(&0u64.to_le_bytes());
            write.extend_from_slice(&7u32.to_le_bytes());
            write.extend_from_slice(b"payload");
            session
                .receive_frame(frame(MessageType::Twrite, 5, &write))
                .unwrap();
            let request = take_engine_request(&mut session, environment.execution(901));
            let first_terminal = terminal(second_engine.execute(request.clone()).await);
            drop(first_terminal);
            let recovered = terminal(first_engine.execute(request).await);
            session.complete(recovered.into_completion()).unwrap();
            let lost_response = take_response(&mut session);
            assert_eq!(lost_response[4], MessageType::Rwrite.to_u8());

            session
                .receive_frame(frame(MessageType::Twrite, 6, &write))
                .unwrap();
            complete_session_effect(
                &second_engine,
                &mut session,
                environment.execution(901),
                MessageType::Rwrite,
            )
            .await;

            let mut setattr = Vec::new();
            setattr.extend_from_slice(&2u32.to_le_bytes());
            setattr.extend_from_slice(&SetattrMask::MODE.bits().to_le_bytes());
            setattr.extend_from_slice(&0o600u32.to_le_bytes());
            setattr.extend_from_slice(&0u32.to_le_bytes());
            setattr.extend_from_slice(&0u32.to_le_bytes());
            setattr.extend_from_slice(&0u64.to_le_bytes());
            for _ in 0..4 {
                setattr.extend_from_slice(&0u64.to_le_bytes());
            }
            session
                .receive_frame(frame(MessageType::Tsetattr, 7, &setattr))
                .unwrap();
            let mut stale_execution = environment.execution(902);
            stale_execution.fence = Some(WriterFence::new(
                environment.fence.scope,
                environment.fence.holder,
                environment.fence.lease_id,
                FencingToken::new(
                    environment
                        .fence
                        .fencing_token
                        .get()
                        .checked_add(1)
                        .unwrap(),
                )
                .unwrap(),
            ));
            let stale_request = take_engine_request(&mut session, stale_execution);
            assert!(matches!(
                second_engine.execute(stale_request.clone()).await,
                EngineOutcome::Unresolved(UnresolvedEngineFailure {
                    failure: crate::ExecutionFailure::Authority(
                        crate::AuthorityFailure::StaleFence
                    ),
                    ..
                })
            ));
            let recovered_request = EngineRequest::new(
                stale_request.operation_id,
                stale_request.request,
                environment.execution(902),
            );
            let recovered = terminal(first_engine.execute(recovered_request).await);
            session.complete(recovered.into_completion()).unwrap();
            expect_response(&mut session, MessageType::Rsetattr);

            let mut link = Vec::new();
            link.extend_from_slice(&1u32.to_le_bytes());
            link.extend_from_slice(&2u32.to_le_bytes());
            link.extend_from_slice(&wire_string("alias"));
            session
                .receive_frame(frame(MessageType::Tlink, 8, &link))
                .unwrap();
            complete_session_effect(
                &second_engine,
                &mut session,
                environment.execution(903),
                MessageType::Rlink,
            )
            .await;

            let mut rename = Vec::new();
            rename.extend_from_slice(&1u32.to_le_bytes());
            rename.extend_from_slice(&wire_string("file"));
            rename.extend_from_slice(&1u32.to_le_bytes());
            rename.extend_from_slice(&wire_string("renamed"));
            session
                .receive_frame(frame(MessageType::Trenameat, 9, &rename))
                .unwrap();
            complete_session_effect(
                &first_engine,
                &mut session,
                environment.execution(904),
                MessageType::Rrenameat,
            )
            .await;

            for (tag, name, mutation) in [(10, "renamed", 905), (11, "alias", 906)] {
                let mut unlink = Vec::new();
                unlink.extend_from_slice(&1u32.to_le_bytes());
                unlink.extend_from_slice(&wire_string(name));
                unlink.extend_from_slice(&0u32.to_le_bytes());
                session
                    .receive_frame(frame(MessageType::Tunlinkat, tag, &unlink))
                    .unwrap();
                complete_session_effect(
                    &second_engine,
                    &mut session,
                    environment.execution(mutation),
                    MessageType::Runlinkat,
                )
                .await;
            }

            let mut read = Vec::new();
            read.extend_from_slice(&2u32.to_le_bytes());
            read.extend_from_slice(&0u64.to_le_bytes());
            read.extend_from_slice(&32u32.to_le_bytes());
            session
                .receive_frame(frame(MessageType::Tread, 12, &read))
                .unwrap();
            let read_request = take_engine_request(
                &mut session,
                ExecutionContext::read_only(environment.client, MutationRetention::new(1_000)),
            );
            let read_terminal = terminal(first_engine.execute(read_request).await);
            session.complete(read_terminal.into_completion()).unwrap();
            let read_response = take_response(&mut session);
            assert_eq!(read_response[4], MessageType::Rread.to_u8());
            assert_eq!(
                u32::from_le_bytes(read_response[7..11].try_into().unwrap()),
                7
            );
            assert_eq!(&read_response[11..], b"payload");
            let mut clunk = Vec::new();
            clunk.extend_from_slice(&2u32.to_le_bytes());
            session
                .receive_frame(frame(MessageType::Tclunk, 13, &clunk))
                .unwrap();
            complete_session_effect(
                &second_engine,
                &mut session,
                environment.execution(907),
                MessageType::Rclunk,
            )
            .await;

            let policy_bump = CommitRequest::new(
                environment.filesystem_id,
                MutationContext::new(
                    MutationId::from_u128(950),
                    RequestFingerprint::blake3(b"session policy bump"),
                    environment.client,
                    MutationRetention::new(1_000),
                ),
                environment.fence,
                vec![Precondition::FilesystemPolicyGeneration { expected: 1 }],
                vec![StateChange::BumpFilesystemPolicyGeneration],
                crate::encode_mutation_result(
                    &FilesystemResult::Released,
                    environment.engine_limits,
                    environment.state_limits,
                )
                .unwrap(),
                environment.state_limits,
            )
            .unwrap();
            assert!(matches!(
                second_engine.state().commit(policy_bump).await.unwrap(),
                CommitOutcome::Committed(_)
            ));
            let mut getattr = Vec::new();
            getattr.extend_from_slice(&1u32.to_le_bytes());
            getattr.extend_from_slice(&w9pt::protocol::GetattrMask::ALL.bits().to_le_bytes());
            session
                .receive_frame(frame(MessageType::Tgetattr, 14, &getattr))
                .unwrap();
            let request = take_engine_request(
                &mut session,
                ExecutionContext::read_only(environment.client, MutationRetention::new(1_000)),
            );
            let terminal = terminal(first_engine.execute(request).await);
            assert!(matches!(
                terminal.result,
                Err(ref error) if error.errno == LinuxErrno::EAGAIN
            ));
            session.complete(terminal.into_completion()).unwrap();
            expect_response(&mut session, MessageType::Rlerror);
        });
    }

    #[test]
    fn committed_write_replays_after_session_response_enqueue_failure() {
        use crate::testing::TestEnvironment;
        use w9pt::protocol::{MessageType, OpenFlags};
        use w9pt::{Completion, Effect, PolicyResult, Session, SessionConfig, SessionContext};

        block_on(async {
            let environment = TestEnvironment::new(false, StorageMethod::Raw).await;
            environment.create_file("queue-file", 1_000).await;
            let engine = environment.engine();
            let mut config = SessionConfig::default();
            config.limits.max_queued_effects = 1;
            let mut session =
                Session::new(config, SessionContext::new(SessionId::new(10))).unwrap();
            negotiate(&mut session);
            let attach_operation = emit_attach(&mut session);
            let attach = engine.resolve_attach(environment.context()).await.unwrap();
            session
                .complete(Completion::Policy {
                    operation_id: attach_operation,
                    result: Ok(PolicyResult::Attached(attach)),
                })
                .unwrap();
            expect_response(&mut session, MessageType::Rattach);

            let mut walk = Vec::new();
            walk.extend_from_slice(&1u32.to_le_bytes());
            walk.extend_from_slice(&2u32.to_le_bytes());
            walk.extend_from_slice(&1u16.to_le_bytes());
            walk.extend_from_slice(&wire_string("queue-file"));
            session
                .receive_frame(frame(MessageType::Twalk, 3, &walk))
                .unwrap();
            complete_session_effect(
                &engine,
                &mut session,
                ExecutionContext::read_only(environment.client, MutationRetention::new(1_000)),
                MessageType::Rwalk,
            )
            .await;

            let mut open = Vec::new();
            open.extend_from_slice(&2u32.to_le_bytes());
            open.extend_from_slice(&OpenFlags::RDWR.bits().to_le_bytes());
            session
                .receive_frame(frame(MessageType::Tlopen, 4, &open))
                .unwrap();
            complete_session_effect(
                &engine,
                &mut session,
                environment.execution(1_001),
                MessageType::Rlopen,
            )
            .await;

            let mut write = Vec::new();
            write.extend_from_slice(&2u32.to_le_bytes());
            write.extend_from_slice(&0u64.to_le_bytes());
            write.extend_from_slice(&1u32.to_le_bytes());
            write.push(b'x');
            session
                .receive_frame(frame(MessageType::Twrite, 5, &write))
                .unwrap();
            let request = take_engine_request(&mut session, environment.execution(1_002));
            let committed = terminal(engine.execute(request.clone()).await);

            let mut invalid_getattr = Vec::new();
            invalid_getattr.extend_from_slice(&99u32.to_le_bytes());
            invalid_getattr
                .extend_from_slice(&w9pt::protocol::GetattrMask::ALL.bits().to_le_bytes());
            session
                .receive_frame(frame(MessageType::Tgetattr, 6, &invalid_getattr))
                .unwrap();
            session.complete(committed.into_completion()).unwrap();
            expect_response(&mut session, MessageType::Rlerror);

            let replayed = terminal(engine.execute(request).await);
            assert_eq!(replayed.result, Ok(FilesystemResult::Written(1)));
            let mut closed = false;
            for _ in 0..8 {
                match session.poll_effect() {
                    Some(Effect::CloseSession { .. }) => {
                        closed = true;
                        break;
                    }
                    Some(_) => {}
                    None => break,
                }
            }
            assert!(closed, "response enqueue failure must close the session");
        });
    }

    fn take_engine_request(
        session: &mut w9pt::Session,
        execution: ExecutionContext,
    ) -> EngineRequest {
        let w9pt::Effect::Filesystem {
            operation_id,
            request,
        } = session.poll_effect().unwrap()
        else {
            panic!("expected filesystem effect")
        };
        EngineRequest::new(operation_id, request, execution)
    }

    fn terminal<S, T, P>(outcome: EngineOutcome<S, T, P>) -> EngineTerminal {
        let EngineOutcome::Terminal(terminal) = outcome else {
            panic!("expected terminal engine outcome")
        };
        terminal
    }

    async fn complete_session_effect(
        engine: &FilesystemEngine<
            w9pt_fs_state::testing::MemoryStateStore,
            MemoryTarget,
            crate::testing::TestPolicy,
            crate::testing::TestIdentities,
        >,
        session: &mut w9pt::Session,
        execution: ExecutionContext,
        expected: w9pt::protocol::MessageType,
    ) {
        let request = take_engine_request(session, execution);
        let terminal = terminal(engine.execute(request).await);
        session.complete(terminal.into_completion()).unwrap();
        expect_response(session, expected);
    }

    fn negotiate(session: &mut w9pt::Session) {
        let mut payload = Vec::new();
        payload.extend_from_slice(&8_192u32.to_le_bytes());
        payload.extend_from_slice(&wire_string("9P2000.L"));
        session
            .receive_frame(frame(
                w9pt::protocol::MessageType::Tversion,
                u16::MAX,
                &payload,
            ))
            .unwrap();
        expect_response(session, w9pt::protocol::MessageType::Rversion);
    }

    fn emit_attach(session: &mut w9pt::Session) -> w9pt::OperationId {
        let mut payload = Vec::new();
        payload.extend_from_slice(&1u32.to_le_bytes());
        payload.extend_from_slice(&u32::MAX.to_le_bytes());
        payload.extend_from_slice(&wire_string("user"));
        payload.extend_from_slice(&wire_string("export"));
        payload.extend_from_slice(&1u32.to_le_bytes());
        session
            .receive_frame(frame(w9pt::protocol::MessageType::Tattach, 2, &payload))
            .unwrap();
        let w9pt::Effect::Policy { operation_id, .. } = session.poll_effect().unwrap() else {
            panic!("expected attach policy effect")
        };
        operation_id
    }

    fn expect_response(session: &mut w9pt::Session, expected: w9pt::protocol::MessageType) {
        let bytes = take_response(session);
        assert_eq!(bytes[4], expected.to_u8());
    }

    fn take_response(session: &mut w9pt::Session) -> Vec<u8> {
        let w9pt::Effect::SendFrame { bytes } = session.poll_effect().unwrap() else {
            panic!("expected response frame")
        };
        bytes
    }

    fn frame(message_type: w9pt::protocol::MessageType, tag: u16, payload: &[u8]) -> Vec<u8> {
        let size = 7usize.checked_add(payload.len()).unwrap();
        let mut bytes = Vec::with_capacity(size);
        bytes.extend_from_slice(&u32::try_from(size).unwrap().to_le_bytes());
        bytes.push(message_type.to_u8());
        bytes.extend_from_slice(&tag.to_le_bytes());
        bytes.extend_from_slice(payload);
        bytes
    }

    fn wire_string(value: &str) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(2 + value.len());
        bytes.extend_from_slice(&u16::try_from(value.len()).unwrap().to_le_bytes());
        bytes.extend_from_slice(value.as_bytes());
        bytes
    }
}
