//! Deterministic semantic-engine fixtures shared by unit tests.

use core::{convert::Infallible, future::Future};

use w9pt::filesystem::{
    CapabilitySet, CreateResult, ExportId, FilesystemOperation, FilesystemRequest,
    FilesystemResult, PrincipalId as WirePrincipalId, RequestContext,
};
use w9pt::protocol::{OpenFlags, SessionId};
use w9pt_fs_state::{
    AcquireLeaseOutcome, AcquireWriterLease, ClientIncarnationId, CommitOutcome, CommitRequest,
    DataGeneration, DirectoryCookie, DirectoryGeneration, FilesystemId, FilesystemRecord,
    FilesystemStateStore, GroupId, InodeAttributeUpdate, InodeData, InodeGeneration, InodeId,
    InodeRecord, InodeTimes, LeaseDeadline, LeaseDuration, LeaseId, LeaseOperationId,
    ManualLeaseClock, MutationContext, MutationRetention, Precondition, PrincipalId,
    PublishContent, QidPath, ReadBatch, ReadConsistency, ReadOutcome, ReadQuery, ReadResult,
    RecordKey, RecordRevision, RequestFingerprint, StateChange, StateLimits, StateRecord,
    StateRevision, UnixTimestamp, WriterFence, WriterIncarnationId, WriterScopeId, WriterTopology,
    testing::{MemoryAuthority, MemoryStateStore},
};
use w9pt_fs_storage::{
    BaseContentIdentity, ContentRef, ContentRepository, CreationDefaults, FileContextScope, FileId,
    MutationId, StorageLimits, StorageMethod, open_committed_context, testing::MemoryTarget,
};

use crate::{
    CanonicalIdentity, EngineLimits, ExecutionContext, ExportGrant, ExportPolicy,
    ExportPolicyRequest, FilesystemEngine, IdentityMappingRequest, IdentityScope, IdentitySource,
    NumericIdentity, ReverseIdentityMappingRequest, encode_mutation_result, inode_id_from_handle,
    object_handle, operations::execute_create,
};

#[derive(Clone)]
pub(crate) struct TestPolicy(pub(crate) ExportGrant);

impl ExportPolicy for TestPolicy {
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
            NumericIdentity::User(_) => CanonicalIdentity::Principal(self.0.principal().clone()),
            NumericIdentity::Group(_) => CanonicalIdentity::Group(self.0.primary_group().clone()),
        }))
    }
}

#[derive(Clone, Copy)]
pub(crate) struct TestIdentities;

impl IdentitySource for TestIdentities {
    type Error = Infallible;

    fn inode_id(&self, scope: IdentityScope) -> Result<InodeId, Self::Error> {
        Ok(InodeId::from_u128(scoped_value(scope, 0x1000)))
    }

    fn open_id(&self, scope: IdentityScope) -> Result<w9pt_fs_state::OpenId, Self::Error> {
        Ok(w9pt_fs_state::OpenId::from_u128(scoped_value(
            scope, 0x2000,
        )))
    }

    fn content_file_id(&self, scope: IdentityScope) -> Result<FileId, Self::Error> {
        Ok(FileId::from_u128(scoped_value(scope, 0x3000)))
    }
}

fn scoped_value(scope: IdentityScope, domain: u128) -> u128 {
    u128::from_be_bytes(*scope.mutation_id.as_bytes())
        .wrapping_mul(16)
        .wrapping_add(u128::from(scope.slot))
        .wrapping_add(domain)
}

#[allow(dead_code)]
pub(crate) struct TestEnvironment {
    pub(crate) authority: MemoryAuthority,
    pub(crate) state: MemoryStateStore,
    pub(crate) target: MemoryTarget,
    pub(crate) repository: ContentRepository<MemoryTarget>,
    pub(crate) policy: TestPolicy,
    pub(crate) identities: TestIdentities,
    pub(crate) filesystem_id: FilesystemId,
    pub(crate) root_id: InodeId,
    pub(crate) fence: WriterFence,
    pub(crate) client: ClientIncarnationId,
    pub(crate) now: UnixTimestamp,
    pub(crate) state_limits: StateLimits,
    pub(crate) engine_limits: EngineLimits,
}

#[allow(dead_code)]
impl TestEnvironment {
    pub(crate) async fn new(read_only: bool, method: StorageMethod) -> Self {
        let state_limits = StateLimits::default();
        let engine_limits = EngineLimits::default();
        let filesystem_id = FilesystemId::from_u128(1);
        let root_id = InodeId::from_u128(2);
        let authority = MemoryAuthority::new(
            WriterTopology::SerializableMultiWriter,
            state_limits,
            ManualLeaseClock::new(LeaseDeadline::new(0)),
        );
        let state = authority.open_client();
        let acquire = AcquireWriterLease::new(
            filesystem_id,
            LeaseOperationId::from_u128(3),
            WriterScopeId::from_u128(4),
            WriterIncarnationId::from_u128(5),
            LeaseId::from_u128(6),
            LeaseDuration::new(1_000).unwrap(),
            state_limits,
        )
        .unwrap();
        let AcquireLeaseOutcome::Granted(lease) =
            state.acquire_writer_lease(acquire).await.unwrap()
        else {
            panic!("test lease not granted")
        };
        let now = UnixTimestamp::new(10, 20).unwrap();
        let owner = PrincipalId::new(b"owner".to_vec(), state_limits).unwrap();
        let group = GroupId::new(b"group".to_vec(), state_limits).unwrap();
        let root = InodeRecord::new(
            root_id,
            QidPath::new(1).unwrap(),
            RecordRevision::new(1).unwrap(),
            0o2775,
            owner.clone(),
            group.clone(),
            InodeTimes {
                accessed: now,
                modified: now,
                changed: now,
                created: now,
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
        let bootstrap = CommitRequest::new(
            filesystem_id,
            MutationContext::new(
                MutationId::from_u128(7),
                RequestFingerprint::blake3(b"test bootstrap"),
                ClientIncarnationId::from_u128(8),
                MutationRetention::new(1_000),
            ),
            lease.fence,
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
            encode_mutation_result(
                &w9pt::filesystem::FilesystemResult::Released,
                engine_limits,
                state_limits,
            )
            .unwrap(),
            state_limits,
        )
        .unwrap();
        assert!(matches!(
            state.commit(bootstrap).await.unwrap(),
            CommitOutcome::Committed(_)
        ));
        let target = MemoryTarget::new();
        let repository = ContentRepository::new(
            target.clone(),
            "semantic-engine-tests",
            CreationDefaults::new(method),
            StorageLimits::default(),
        )
        .unwrap();
        let policy = TestPolicy(
            ExportGrant::new(
                filesystem_id,
                root_id,
                owner,
                group,
                Vec::new(),
                1,
                1,
                false,
                read_only,
                1,
                CapabilitySet::ALL,
                engine_limits,
            )
            .unwrap(),
        );
        Self {
            authority,
            state,
            target,
            repository,
            policy,
            identities: TestIdentities,
            filesystem_id,
            root_id,
            fence: lease.fence,
            client: ClientIncarnationId::from_u128(9),
            now,
            state_limits,
            engine_limits,
        }
    }

    pub(crate) fn engine(
        &self,
    ) -> FilesystemEngine<MemoryStateStore, MemoryTarget, TestPolicy, TestIdentities> {
        FilesystemEngine::new(
            self.state.clone(),
            self.repository.clone(),
            self.policy.clone(),
            self.identities,
            self.engine_limits,
        )
    }

    pub(crate) fn context(&self) -> RequestContext {
        RequestContext::new(
            SessionId::new(10),
            WirePrincipalId::new("principal"),
            ExportId::new("export"),
        )
    }

    pub(crate) fn execution(&self, mutation: u128) -> ExecutionContext {
        ExecutionContext::new(
            self.client,
            Some(MutationId::from_u128(mutation)),
            MutationRetention::new(1_000),
            Some(self.fence),
            Some(self.now),
        )
    }

    pub(crate) async fn create_file(&self, name: &str, mutation: u128) -> CreateResult {
        let request = FilesystemRequest::new(
            self.context(),
            FilesystemOperation::Create {
                directory: object_handle(self.root_id),
                name: name.into(),
                flags: OpenFlags::RDWR,
                mode: 0o660,
                gid: 1,
            },
        );
        let FilesystemResult::Created(created) = execute_create(
            &self.state,
            &self.repository,
            &self.policy,
            &self.identities,
            self.engine_limits,
            request,
            self.execution(mutation),
        )
        .await
        .unwrap() else {
            panic!("unexpected create result")
        };
        created
    }

    pub(crate) async fn publish_new_content(
        &self,
        object: w9pt::filesystem::ObjectHandle,
        bytes: &[u8],
        mutation_value: u128,
    ) -> ContentRef {
        let inode_id = inode_id_from_handle(object);
        let read = ReadBatch::new(
            self.filesystem_id,
            ReadConsistency::LatestLinearizable,
            vec![ReadQuery::InodeWithContentMetadata(inode_id)],
            self.state_limits,
        )
        .unwrap();
        let ReadOutcome::Snapshot(snapshot) = self.state.read(read).await.unwrap() else {
            panic!("new file state unavailable")
        };
        let ReadResult::InodeWithContentMetadata {
            inode: Some(inode),
            metadata: Some(metadata),
            ..
        } = &snapshot.results()[0]
        else {
            panic!("new file content context unavailable")
        };
        let context = open_committed_context(
            FileContextScope::new(
                *self.filesystem_id.as_bytes(),
                *inode_id.as_bytes(),
                metadata.content_file_id(),
                metadata.context_id(),
            ),
            metadata.policy_format(),
            metadata.policy_bytes(),
            metadata.key_commitment().copied(),
            metadata.wrapped_key_bytes(),
            metadata.revision().get(),
            None,
        )
        .unwrap();
        let mutation_id = MutationId::from_u128(mutation_value);
        let prepared = self
            .repository
            .prepare_write_from_new_with_context(&context, mutation_id, 0, 0, bytes)
            .await
            .unwrap();
        let content = prepared.content().clone();
        let commit = CommitRequest::new(
            self.filesystem_id,
            MutationContext::new(
                mutation_id,
                RequestFingerprint::blake3(&mutation_value.to_be_bytes()),
                self.client,
                MutationRetention::new(1_000),
            ),
            self.fence,
            vec![
                Precondition::ContentBase {
                    inode_id,
                    expected: BaseContentIdentity::NEW_FILE,
                },
                Precondition::RecordRevision {
                    key: RecordKey::ContentMetadata(self.filesystem_id, metadata.content_file_id()),
                    expected: metadata.revision(),
                },
            ],
            vec![StateChange::PublishContent(PublishContent {
                inode_id,
                expected_base: BaseContentIdentity::NEW_FILE,
                logical_size: content.logical_size(),
                data_generation: DataGeneration::new(content.generation()).unwrap(),
                inode_generation: inode.inode_generation().checked_next().unwrap(),
                prepared,
                attributes: InodeAttributeUpdate {
                    modified: Some(self.now),
                    changed: Some(self.now),
                    ..InodeAttributeUpdate::default()
                },
            })],
            encode_mutation_result(
                &FilesystemResult::Released,
                self.engine_limits,
                self.state_limits,
            )
            .unwrap(),
            self.state_limits,
        )
        .unwrap();
        assert!(matches!(
            self.state.commit(commit).await.unwrap(),
            CommitOutcome::Committed(_)
        ));
        content
    }
}
