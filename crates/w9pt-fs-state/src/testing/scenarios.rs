use crate::{
    AcquireLeaseOutcome, AcquireWriterLease, ClientIncarnationId, CommitOutcome, CommitRequest,
    DataGeneration, DirectoryCookie, DirectoryEntryRecord, DirectoryGeneration, EntryName,
    FilesystemId, FilesystemRecord, GroupId, InodeData, InodeGeneration, InodeId, InodeRecord,
    InodeTimes, LeaseDeadline, LeaseDuration, LeaseId, LeaseOperationId, LockGeneration, LockId,
    LockKind, LockOwner, LockRange, LockRecord, ManualLeaseClock, MutationContext, MutationResult,
    MutationResultKind, MutationRetention, OpenAccess, OpenId, OpenPinRecord, OpenRecord,
    OrphanRecord, Precondition, PrincipalId, PublishContent, QidPath, ReadBatch, ReadConsistency,
    ReadOutcome, ReadQuery, ReadResult, RecordKey, RecordRevision, ResultFormatVersion,
    StateChange, StateLimits, StateRecord, StateRevision, UnixTimestamp, WriterFence,
    WriterIncarnationId, WriterScopeId, WriterTopology, XattrName, XattrStagingId,
    XattrStagingRecord, XattrValue,
};

use super::MemoryAuthority;

struct Harness {
    authority: MemoryAuthority,
    client: super::MemoryStateStore,
    limits: StateLimits,
    filesystem_id: FilesystemId,
    root_id: InodeId,
    file_id: InodeId,
    content_file_id: w9pt_fs_storage::FileId,
    first_open: OpenId,
    second_open: OpenId,
    first_client: ClientIncarnationId,
    second_client: ClientIncarnationId,
    fence: WriterFence,
    next_mutation: u128,
}

impl Harness {
    fn new() -> Self {
        let limits = StateLimits::default();
        let authority = MemoryAuthority::new(
            WriterTopology::SerializableMultiWriter,
            limits,
            ManualLeaseClock::new(LeaseDeadline::new(0)),
        );
        let client = authority.open_client();
        let filesystem_id = FilesystemId::from_u128(1);
        let root_id = InodeId::from_u128(2);
        let file_id = InodeId::from_u128(3);
        let content_file_id = w9pt_fs_storage::FileId::from_u128(3);
        let first_open = OpenId::from_u128(4);
        let second_open = OpenId::from_u128(5);
        let first_client = ClientIncarnationId::from_u128(6);
        let second_client = ClientIncarnationId::from_u128(7);
        let acquire = AcquireWriterLease::new(
            filesystem_id,
            LeaseOperationId::from_u128(8),
            WriterScopeId::from_u128(9),
            WriterIncarnationId::from_u128(10),
            LeaseId::from_u128(11),
            LeaseDuration::new(1_000).unwrap(),
            limits,
        )
        .unwrap();
        let AcquireLeaseOutcome::Granted(grant) =
            w9pt_fs_storage::testing::block_on(client.acquire_lease_request(acquire)).unwrap()
        else {
            panic!("test lease must be granted");
        };
        let revision = RecordRevision::new(1).unwrap();
        let root = root_record(root_id, 1, limits);
        let file = file_record(
            file_id,
            QidPath::new(2).unwrap(),
            content_file_id,
            1,
            1,
            None,
            identity_times(0),
            limits,
        );
        let name = EntryName::new(b"file".to_vec(), limits).unwrap();
        let filesystem = FilesystemRecord::new(
            filesystem_id,
            StateRevision::new(1).unwrap(),
            revision,
            root_id,
            QidPath::new(3).unwrap(),
            DirectoryCookie::new(3),
            1,
        )
        .unwrap();
        let open_a = OpenRecord::new(
            first_open,
            file_id,
            first_client,
            OpenAccess::ReadWrite,
            false,
            InodeGeneration::new(1).unwrap(),
            revision,
        );
        let open_b = OpenRecord::new(
            second_open,
            file_id,
            second_client,
            OpenAccess::ReadWrite,
            false,
            InodeGeneration::new(1).unwrap(),
            revision,
        );
        let mut harness = Self {
            authority,
            client,
            limits,
            filesystem_id,
            root_id,
            file_id,
            content_file_id,
            first_open,
            second_open,
            first_client,
            second_client,
            fence: grant.fence,
            next_mutation: 100,
        };
        assert!(matches!(
            harness.commit(vec![
                StateChange::Insert {
                    key: RecordKey::Filesystem(filesystem_id),
                    record: StateRecord::Filesystem(filesystem),
                },
                StateChange::Insert {
                    key: RecordKey::Inode(filesystem_id, root_id),
                    record: StateRecord::Inode(root),
                },
                StateChange::Insert {
                    key: RecordKey::Inode(filesystem_id, file_id),
                    record: StateRecord::Inode(file),
                },
                StateChange::Insert {
                    key: RecordKey::DirectoryEntry(filesystem_id, root_id, name.clone()),
                    record: StateRecord::DirectoryEntry(
                        DirectoryEntryRecord::new(
                            root_id,
                            name,
                            DirectoryCookie::new(1),
                            file_id,
                            revision,
                        )
                        .unwrap(),
                    ),
                },
                StateChange::Insert {
                    key: RecordKey::Open(filesystem_id, first_open),
                    record: StateRecord::Open(open_a),
                },
                StateChange::Insert {
                    key: RecordKey::OpenPin(filesystem_id, file_id, first_open),
                    record: StateRecord::OpenPin(OpenPinRecord::new(file_id, first_open, revision)),
                },
                StateChange::Insert {
                    key: RecordKey::Open(filesystem_id, second_open),
                    record: StateRecord::Open(open_b),
                },
                StateChange::Insert {
                    key: RecordKey::OpenPin(filesystem_id, file_id, second_open),
                    record: StateRecord::OpenPin(OpenPinRecord::new(
                        file_id,
                        second_open,
                        revision
                    )),
                },
            ]),
            CommitOutcome::Committed(_)
        ));
        harness
    }

    fn commit(&mut self, changes: Vec<StateChange>) -> CommitOutcome {
        self.commit_with_preconditions(vec![], changes)
    }

    fn commit_with_preconditions(
        &mut self,
        preconditions: Vec<Precondition>,
        changes: Vec<StateChange>,
    ) -> CommitOutcome {
        let mutation_number = self.next_mutation;
        self.next_mutation += 1;
        let request = CommitRequest::new(
            self.filesystem_id,
            MutationContext::new(
                w9pt_fs_storage::MutationId::from_u128(mutation_number),
                crate::RequestFingerprint::blake3(&mutation_number.to_be_bytes()),
                ClientIncarnationId::from_u128(12),
                MutationRetention::new(1_000),
            ),
            self.fence,
            preconditions,
            changes,
            MutationResult::new(
                MutationResultKind::new(1).unwrap(),
                ResultFormatVersion::new(1).unwrap(),
                b"ok".to_vec(),
                self.limits,
            )
            .unwrap(),
            self.limits,
        )
        .unwrap();
        w9pt_fs_storage::testing::block_on(self.client.commit_request(request)).unwrap()
    }

    fn read(&self, query: ReadQuery) -> Option<StateRecord> {
        let batch = ReadBatch::new(
            self.filesystem_id,
            ReadConsistency::LatestLinearizable,
            vec![query],
            self.limits,
        )
        .unwrap();
        let ReadOutcome::Snapshot(snapshot) =
            w9pt_fs_storage::testing::block_on(self.client.read_request(batch)).unwrap()
        else {
            panic!("expected snapshot");
        };
        let ReadResult::Point { record, .. } = &snapshot.results()[0] else {
            panic!("expected point result");
        };
        record.as_deref().cloned()
    }
}

fn identity_times(seconds: i64) -> InodeTimes {
    let timestamp = UnixTimestamp::new(seconds, 0).unwrap();
    InodeTimes {
        accessed: timestamp,
        modified: timestamp,
        changed: timestamp,
        created: timestamp,
    }
}

fn root_record(root_id: InodeId, generation: u64, limits: StateLimits) -> InodeRecord {
    InodeRecord::new(
        root_id,
        QidPath::new(1).unwrap(),
        RecordRevision::new(1).unwrap(),
        0o755,
        PrincipalId::new(b"root".to_vec(), limits).unwrap(),
        GroupId::new(b"root".to_vec(), limits).unwrap(),
        identity_times(0),
        0,
        1,
        InodeGeneration::new(generation).unwrap(),
        InodeData::Directory {
            generation: DirectoryGeneration::new(generation).unwrap(),
            parent_inode_id: root_id,
        },
    )
    .unwrap()
}

#[allow(clippy::too_many_arguments)]
fn file_record(
    inode_id: InodeId,
    qid_path: QidPath,
    content_file_id: w9pt_fs_storage::FileId,
    links: u64,
    inode_generation: u64,
    content: Option<w9pt_fs_storage::ContentRef>,
    times: InodeTimes,
    limits: StateLimits,
) -> InodeRecord {
    let logical_size = content
        .as_ref()
        .map_or(0, w9pt_fs_storage::ContentRef::logical_size);
    let data_generation = content
        .as_ref()
        .map_or(0, w9pt_fs_storage::ContentRef::generation);
    InodeRecord::new(
        inode_id,
        qid_path,
        RecordRevision::new(1).unwrap(),
        0o644,
        PrincipalId::new(b"owner".to_vec(), limits).unwrap(),
        GroupId::new(b"group".to_vec(), limits).unwrap(),
        times,
        logical_size,
        links,
        InodeGeneration::new(inode_generation).unwrap(),
        InodeData::RegularFile {
            content_file_id,
            content,
            data_generation,
        },
    )
    .unwrap()
}

#[test]
fn create_rename_link_and_unlink_are_atomic_record_sets() {
    let mut harness = Harness::new();
    let inode = InodeId::from_u128(20);
    let content_file = w9pt_fs_storage::FileId::from_u128(20);
    let created = EntryName::new(b"created".to_vec(), harness.limits).unwrap();
    assert!(matches!(
        harness.commit(vec![
            StateChange::Insert {
                key: RecordKey::Inode(harness.filesystem_id, inode),
                record: StateRecord::Inode(file_record(
                    inode,
                    QidPath::new(3).unwrap(),
                    content_file,
                    1,
                    1,
                    None,
                    identity_times(0),
                    harness.limits,
                )),
            },
            StateChange::Insert {
                key: RecordKey::DirectoryEntry(
                    harness.filesystem_id,
                    harness.root_id,
                    created.clone(),
                ),
                record: StateRecord::DirectoryEntry(
                    DirectoryEntryRecord::new(
                        harness.root_id,
                        created.clone(),
                        DirectoryCookie::new(3),
                        inode,
                        RecordRevision::new(1).unwrap(),
                    )
                    .unwrap(),
                ),
            },
            StateChange::Replace {
                key: RecordKey::Inode(harness.filesystem_id, harness.root_id),
                record: StateRecord::Inode(root_record(harness.root_id, 2, harness.limits)),
            },
            StateChange::AdvanceDirectoryCookie {
                count: core::num::NonZeroU64::new(1).unwrap(),
            },
            StateChange::AdvanceQidPath {
                count: core::num::NonZeroU64::new(1).unwrap(),
            },
        ]),
        CommitOutcome::Committed(_)
    ));
    let Some(StateRecord::Filesystem(filesystem)) = harness.read(ReadQuery::Filesystem) else {
        panic!("filesystem header must remain");
    };
    assert_eq!(filesystem.next_qid_path(), QidPath::new(4).unwrap());

    let renamed = EntryName::new(b"renamed".to_vec(), harness.limits).unwrap();
    assert!(matches!(
        harness.commit(vec![
            StateChange::Delete(RecordKey::DirectoryEntry(
                harness.filesystem_id,
                harness.root_id,
                created,
            )),
            StateChange::Insert {
                key: RecordKey::DirectoryEntry(
                    harness.filesystem_id,
                    harness.root_id,
                    renamed.clone(),
                ),
                record: StateRecord::DirectoryEntry(
                    DirectoryEntryRecord::new(
                        harness.root_id,
                        renamed.clone(),
                        DirectoryCookie::new(3),
                        inode,
                        RecordRevision::new(1).unwrap(),
                    )
                    .unwrap(),
                ),
            },
            StateChange::Replace {
                key: RecordKey::Inode(harness.filesystem_id, harness.root_id),
                record: StateRecord::Inode(root_record(harness.root_id, 3, harness.limits)),
            },
        ]),
        CommitOutcome::Committed(_)
    ));

    let alias = EntryName::new(b"alias".to_vec(), harness.limits).unwrap();
    assert!(matches!(
        harness.commit(vec![
            StateChange::Insert {
                key: RecordKey::DirectoryEntry(
                    harness.filesystem_id,
                    harness.root_id,
                    alias.clone(),
                ),
                record: StateRecord::DirectoryEntry(
                    DirectoryEntryRecord::new(
                        harness.root_id,
                        alias.clone(),
                        DirectoryCookie::new(4),
                        inode,
                        RecordRevision::new(1).unwrap(),
                    )
                    .unwrap(),
                ),
            },
            StateChange::Replace {
                key: RecordKey::Inode(harness.filesystem_id, inode),
                record: StateRecord::Inode(file_record(
                    inode,
                    QidPath::new(3).unwrap(),
                    content_file,
                    2,
                    2,
                    None,
                    identity_times(0),
                    harness.limits,
                )),
            },
            StateChange::Replace {
                key: RecordKey::Inode(harness.filesystem_id, harness.root_id),
                record: StateRecord::Inode(root_record(harness.root_id, 4, harness.limits)),
            },
            StateChange::AdvanceDirectoryCookie {
                count: core::num::NonZeroU64::new(1).unwrap(),
            },
        ]),
        CommitOutcome::Committed(_)
    ));
    assert!(matches!(
        harness.commit(vec![
            StateChange::Delete(RecordKey::DirectoryEntry(
                harness.filesystem_id,
                harness.root_id,
                alias,
            )),
            StateChange::Replace {
                key: RecordKey::Inode(harness.filesystem_id, inode),
                record: StateRecord::Inode(file_record(
                    inode,
                    QidPath::new(3).unwrap(),
                    content_file,
                    1,
                    3,
                    None,
                    identity_times(0),
                    harness.limits,
                )),
            },
            StateChange::Replace {
                key: RecordKey::Inode(harness.filesystem_id, harness.root_id),
                record: StateRecord::Inode(root_record(harness.root_id, 5, harness.limits)),
            },
        ]),
        CommitOutcome::Committed(_)
    ));
    let Some(StateRecord::Inode(file)) = harness.read(ReadQuery::Inode(inode)) else {
        panic!("linked inode must remain");
    };
    assert_eq!(file.link_count(), 1);
    let Some(StateRecord::DirectoryEntry(entry)) = harness.read(ReadQuery::DirectoryEntry {
        parent_inode_id: harness.root_id,
        name: renamed,
    }) else {
        panic!("renamed entry must remain");
    };
    assert_eq!(entry.child_inode_id(), inode);
}

#[test]
fn qid_lookup_is_filesystem_scoped_and_allocated_paths_cannot_be_reused() {
    let mut harness = Harness::new();
    let read = |filesystem_id, qid_path| {
        let request = ReadBatch::new(
            filesystem_id,
            ReadConsistency::LatestLinearizable,
            vec![ReadQuery::InodeByQidPath(qid_path)],
            harness.limits,
        )
        .unwrap();
        let ReadOutcome::Snapshot(snapshot) =
            w9pt_fs_storage::testing::block_on(harness.client.read_request(request)).unwrap()
        else {
            panic!("expected QID lookup snapshot");
        };
        snapshot.results()[0].clone()
    };

    assert!(matches!(
        read(harness.filesystem_id, QidPath::new(2).unwrap()),
        ReadResult::InodeByQidPath { qid_path, inode: Some(inode) }
            if qid_path == QidPath::new(2).unwrap() && inode.inode_id() == harness.file_id
    ));
    assert!(matches!(
        read(FilesystemId::from_u128(999), QidPath::new(2).unwrap()),
        ReadResult::InodeByQidPath { inode: None, .. }
    ));

    let reused_inode = InodeId::from_u128(30);
    assert!(matches!(
        harness.commit(vec![
            StateChange::Insert {
                key: RecordKey::Inode(harness.filesystem_id, reused_inode),
                record: StateRecord::Inode(file_record(
                    reused_inode,
                    QidPath::new(1).unwrap(),
                    w9pt_fs_storage::FileId::from_u128(30),
                    1,
                    1,
                    None,
                    identity_times(0),
                    harness.limits,
                )),
            },
            StateChange::AdvanceQidPath {
                count: core::num::NonZeroU64::new(1).unwrap(),
            },
        ]),
        CommitOutcome::MalformedRequest(crate::MalformedCommit::QidPathAllocation)
    ));
}

#[test]
fn open_pin_count_is_fixed_size_and_preconditioned_at_commit() {
    let mut harness = Harness::new();
    let request = ReadBatch::new(
        harness.filesystem_id,
        ReadConsistency::LatestLinearizable,
        vec![ReadQuery::OpenPinCount(harness.file_id)],
        harness.limits,
    )
    .unwrap();
    let ReadOutcome::Snapshot(snapshot) =
        w9pt_fs_storage::testing::block_on(harness.client.read_request(request)).unwrap()
    else {
        panic!("expected open-pin count snapshot");
    };
    assert_eq!(
        snapshot.results()[0],
        ReadResult::OpenPinCount {
            inode_id: harness.file_id,
            count: 2,
        }
    );

    assert!(matches!(
        harness.commit_with_preconditions(
            vec![Precondition::OpenPinCount {
                inode_id: harness.file_id,
                expected: 2,
            }],
            vec![StateChange::BumpInodeGeneration(harness.file_id)],
        ),
        CommitOutcome::Committed(_)
    ));
    assert!(matches!(
        harness.commit_with_preconditions(
            vec![Precondition::OpenPinCount {
                inode_id: harness.file_id,
                expected: 3,
            }],
            vec![StateChange::BumpInodeGeneration(harness.file_id)],
        ),
        CommitOutcome::Conflict(crate::CommitConflict {
            kind: crate::CommitConflictKind::OpenPinCount,
            ..
        })
    ));
}

#[test]
fn policy_generation_precondition_rejects_stale_authorization() {
    let mut harness = Harness::new();
    assert!(matches!(
        harness.commit(vec![StateChange::BumpFilesystemPolicyGeneration]),
        CommitOutcome::Committed(_)
    ));
    assert!(matches!(
        harness.commit_with_preconditions(
            vec![Precondition::FilesystemPolicyGeneration { expected: 1 }],
            vec![StateChange::BumpInodeGeneration(harness.file_id)],
        ),
        CommitOutcome::Conflict(crate::CommitConflict {
            kind: crate::CommitConflictKind::FilesystemPolicyGeneration,
            ..
        })
    ));
    assert!(matches!(
        harness.commit_with_preconditions(
            vec![Precondition::FilesystemPolicyGeneration { expected: 2 }],
            vec![StateChange::BumpInodeGeneration(harness.file_id)],
        ),
        CommitOutcome::Committed(_)
    ));
}

#[test]
fn prepared_content_and_open_unlinked_lifetime_publish_atomically() {
    let mut harness = Harness::new();
    let repository = w9pt_fs_storage::ContentRepository::new(
        w9pt_fs_storage::testing::MemoryTarget::new(),
        "scenario",
        w9pt_fs_storage::CreationDefaults::new(w9pt_fs_storage::StorageMethod::BlockSplit),
        w9pt_fs_storage::StorageLimits::default(),
    )
    .unwrap();
    let mutation_id = w9pt_fs_storage::MutationId::from_u128(harness.next_mutation);
    let prepared = w9pt_fs_storage::testing::block_on(repository.prepare_create(
        harness.content_file_id,
        mutation_id,
        0,
        b"content",
    ))
    .unwrap();
    let mutation = MutationContext::new(
        mutation_id,
        crate::RequestFingerprint::blake3(b"publish"),
        ClientIncarnationId::from_u128(12),
        MutationRetention::new(1_000),
    );
    let changed_time = UnixTimestamp::new(1, 0).unwrap();
    let publication = PublishContent {
        inode_id: harness.file_id,
        expected_base: w9pt_fs_storage::BaseContentIdentity::NEW_FILE,
        logical_size: prepared.content().logical_size(),
        data_generation: DataGeneration::new(prepared.content().generation()).unwrap(),
        prepared: prepared.clone(),
        inode_generation: InodeGeneration::new(2).unwrap(),
        attributes: crate::InodeAttributeUpdate {
            mode: Some(0o600),
            modified: Some(changed_time),
            changed: Some(changed_time),
            ..crate::InodeAttributeUpdate::default()
        },
    };
    let result = MutationResult::new(
        MutationResultKind::new(1).unwrap(),
        ResultFormatVersion::new(1).unwrap(),
        b"published".to_vec(),
        harness.limits,
    )
    .unwrap();
    let conflict = CommitRequest::new(
        harness.filesystem_id,
        mutation,
        harness.fence,
        vec![Precondition::InodeGeneration {
            inode_id: harness.file_id,
            expected: InodeGeneration::new(2).unwrap(),
        }],
        vec![StateChange::PublishContent(publication.clone())],
        result.clone(),
        harness.limits,
    )
    .unwrap();
    assert!(matches!(
        w9pt_fs_storage::testing::block_on(harness.client.commit_request(conflict)).unwrap(),
        CommitOutcome::Conflict(crate::CommitConflict {
            kind: crate::CommitConflictKind::InodeGeneration,
            ..
        })
    ));
    assert!(matches!(
        harness.read(ReadQuery::Inode(harness.file_id)),
        Some(StateRecord::Inode(inode))
            if inode.content().is_none() && inode.mode() == 0o644
    ));

    let request = CommitRequest::new(
        harness.filesystem_id,
        mutation,
        harness.fence,
        vec![Precondition::InodeGeneration {
            inode_id: harness.file_id,
            expected: InodeGeneration::new(1).unwrap(),
        }],
        vec![StateChange::PublishContent(publication)],
        result,
        harness.limits,
    )
    .unwrap();
    harness.next_mutation += 1;
    assert!(matches!(
        w9pt_fs_storage::testing::block_on(harness.client.commit_request(request)).unwrap(),
        CommitOutcome::Committed(_)
    ));
    assert!(matches!(
        harness.read(ReadQuery::Inode(harness.file_id)),
        Some(StateRecord::Inode(inode))
            if inode.mode() == 0o600
                && inode.times().accessed == identity_times(0).accessed
                && inode.times().modified == changed_time
    ));
    let name = EntryName::new(b"file".to_vec(), harness.limits).unwrap();
    let published = file_record(
        harness.file_id,
        QidPath::new(2).unwrap(),
        harness.content_file_id,
        0,
        3,
        Some(prepared.content().clone()),
        identity_times(2),
        harness.limits,
    );
    assert!(matches!(
        harness.commit(vec![
            StateChange::Delete(RecordKey::DirectoryEntry(
                harness.filesystem_id,
                harness.root_id,
                name,
            )),
            StateChange::Replace {
                key: RecordKey::Inode(harness.filesystem_id, harness.file_id),
                record: StateRecord::Inode(published),
            },
            StateChange::Insert {
                key: RecordKey::Orphan(harness.filesystem_id, harness.file_id),
                record: StateRecord::Orphan(
                    OrphanRecord::new(
                        harness.file_id,
                        2,
                        StateRevision::new(1).unwrap(),
                        RecordRevision::new(1).unwrap(),
                    )
                    .unwrap(),
                ),
            },
            StateChange::Replace {
                key: RecordKey::Inode(harness.filesystem_id, harness.root_id),
                record: StateRecord::Inode(root_record(harness.root_id, 2, harness.limits)),
            },
        ]),
        CommitOutcome::Committed(_)
    ));
    assert!(matches!(
        harness.read(ReadQuery::Orphan(harness.file_id)),
        Some(StateRecord::Orphan(orphan)) if orphan.open_pin_count() == 2
    ));
    assert!(matches!(
        harness.commit(vec![
            StateChange::Delete(RecordKey::Open(harness.filesystem_id, harness.first_open,)),
            StateChange::Delete(RecordKey::OpenPin(
                harness.filesystem_id,
                harness.file_id,
                harness.first_open,
            )),
            StateChange::Replace {
                key: RecordKey::Orphan(harness.filesystem_id, harness.file_id),
                record: StateRecord::Orphan(
                    OrphanRecord::new(
                        harness.file_id,
                        1,
                        StateRevision::new(1).unwrap(),
                        RecordRevision::new(1).unwrap(),
                    )
                    .unwrap(),
                ),
            },
        ]),
        CommitOutcome::Committed(_)
    ));
    assert!(matches!(
        harness.commit(vec![
            StateChange::Delete(RecordKey::Open(harness.filesystem_id, harness.second_open,)),
            StateChange::Delete(RecordKey::OpenPin(
                harness.filesystem_id,
                harness.file_id,
                harness.second_open,
            )),
            StateChange::Delete(RecordKey::Orphan(harness.filesystem_id, harness.file_id,)),
            StateChange::Delete(RecordKey::Inode(harness.filesystem_id, harness.file_id,)),
        ]),
        CommitOutcome::Committed(_)
    ));
    assert!(harness.read(ReadQuery::Inode(harness.file_id)).is_none());
}

#[test]
fn locks_xattrs_and_simultaneous_attributes_remain_atomic() {
    let mut harness = Harness::new();
    let first_lock = LockRecord::new(
        LockId::from_u128(30),
        harness.file_id,
        LockRange::finite(0, 10).unwrap(),
        LockKind::Exclusive,
        LockOwner::new(harness.first_client, harness.first_open),
        LockGeneration::new(1).unwrap(),
        RecordRevision::new(1).unwrap(),
    );
    assert!(matches!(
        harness.commit(vec![StateChange::Insert {
            key: RecordKey::Lock(harness.filesystem_id, harness.file_id, first_lock.lock_id(),),
            record: StateRecord::Lock(first_lock),
        }]),
        CommitOutcome::Committed(_)
    ));
    let second_lock = LockRecord::new(
        LockId::from_u128(31),
        harness.file_id,
        LockRange::finite(5, 15).unwrap(),
        LockKind::Shared,
        LockOwner::new(harness.second_client, harness.second_open),
        LockGeneration::new(1).unwrap(),
        RecordRevision::new(1).unwrap(),
    );
    assert!(matches!(
        harness.commit(vec![StateChange::Insert {
            key: RecordKey::Lock(
                harness.filesystem_id,
                harness.file_id,
                second_lock.lock_id(),
            ),
            record: StateRecord::Lock(second_lock),
        }]),
        CommitOutcome::Conflict(_)
    ));

    let staging_id = XattrStagingId::from_u128(40);
    let xattr_name = XattrName::new(b"user.test".to_vec(), harness.limits).unwrap();
    let value = XattrValue::new(b"value".to_vec(), harness.limits).unwrap();
    let staging = XattrStagingRecord::new(
        staging_id,
        harness.file_id,
        xattr_name.clone(),
        5,
        value,
        RecordRevision::new(1).unwrap(),
        harness.limits,
    )
    .unwrap();
    assert!(matches!(
        harness.commit(vec![StateChange::Insert {
            key: RecordKey::XattrStaging(harness.filesystem_id, staging_id),
            record: StateRecord::XattrStaging(staging),
        }]),
        CommitOutcome::Committed(_)
    ));
    assert!(matches!(
        harness.commit(vec![StateChange::PublishXattrStaging(
            crate::PublishXattrStaging {
                staging_id,
                inode_id: harness.file_id,
                name: xattr_name.clone(),
            },
        )]),
        CommitOutcome::Committed(_)
    ));

    let changed_times = identity_times(9);
    let changed = InodeRecord::new(
        harness.file_id,
        QidPath::new(2).unwrap(),
        RecordRevision::new(1).unwrap(),
        0o600,
        PrincipalId::new(b"alice".to_vec(), harness.limits).unwrap(),
        GroupId::new(b"staff".to_vec(), harness.limits).unwrap(),
        changed_times,
        0,
        1,
        InodeGeneration::new(2).unwrap(),
        InodeData::RegularFile {
            content_file_id: harness.content_file_id,
            content: None,
            data_generation: 0,
        },
    )
    .unwrap();
    assert!(matches!(
        harness.commit(vec![StateChange::Replace {
            key: RecordKey::Inode(harness.filesystem_id, harness.file_id),
            record: StateRecord::Inode(changed),
        }]),
        CommitOutcome::Committed(_)
    ));
    let Some(StateRecord::Inode(observed)) = harness.read(ReadQuery::Inode(harness.file_id)) else {
        panic!("file must remain");
    };
    assert_eq!(observed.mode(), 0o600);
    assert_eq!(observed.owner().as_bytes(), b"alice");
    assert_eq!(observed.group().as_bytes(), b"staff");
    assert_eq!(observed.times(), changed_times);
    assert_eq!(observed.inode_generation().get(), 2);
    assert!(matches!(
        harness.read(ReadQuery::Xattr {
            inode_id: harness.file_id,
            name: xattr_name,
        }),
        Some(StateRecord::Xattr(_))
    ));
    assert!(harness.read(ReadQuery::XattrStaging(staging_id)).is_none());
    assert!(harness.authority.trace().unwrap().len() >= 2);
}

#[test]
fn deleted_directory_cookies_cannot_be_reallocated() {
    let mut harness = Harness::new();
    let original_name = EntryName::new(b"file".to_vec(), harness.limits).unwrap();
    assert!(matches!(
        harness.commit(vec![
            StateChange::Delete(RecordKey::DirectoryEntry(
                harness.filesystem_id,
                harness.root_id,
                original_name,
            )),
            StateChange::Replace {
                key: RecordKey::Inode(harness.filesystem_id, harness.file_id),
                record: StateRecord::Inode(file_record(
                    harness.file_id,
                    QidPath::new(2).unwrap(),
                    harness.content_file_id,
                    0,
                    2,
                    None,
                    identity_times(1),
                    harness.limits,
                )),
            },
            StateChange::Insert {
                key: RecordKey::Orphan(harness.filesystem_id, harness.file_id),
                record: StateRecord::Orphan(
                    OrphanRecord::new(
                        harness.file_id,
                        2,
                        StateRevision::new(1).unwrap(),
                        RecordRevision::new(1).unwrap(),
                    )
                    .unwrap(),
                ),
            },
            StateChange::Replace {
                key: RecordKey::Inode(harness.filesystem_id, harness.root_id),
                record: StateRecord::Inode(root_record(harness.root_id, 2, harness.limits)),
            },
        ]),
        CommitOutcome::Committed(_)
    ));
    let reused = EntryName::new(b"reused".to_vec(), harness.limits).unwrap();
    assert!(matches!(
        harness.commit(vec![
            StateChange::Insert {
                key: RecordKey::DirectoryEntry(
                    harness.filesystem_id,
                    harness.root_id,
                    reused.clone(),
                ),
                record: StateRecord::DirectoryEntry(
                    DirectoryEntryRecord::new(
                        harness.root_id,
                        reused,
                        DirectoryCookie::new(1),
                        harness.file_id,
                        RecordRevision::new(1).unwrap(),
                    )
                    .unwrap(),
                ),
            },
            StateChange::Replace {
                key: RecordKey::Inode(harness.filesystem_id, harness.file_id),
                record: StateRecord::Inode(file_record(
                    harness.file_id,
                    QidPath::new(2).unwrap(),
                    harness.content_file_id,
                    1,
                    3,
                    None,
                    identity_times(2),
                    harness.limits,
                )),
            },
            StateChange::Delete(RecordKey::Orphan(harness.filesystem_id, harness.file_id,)),
            StateChange::Replace {
                key: RecordKey::Inode(harness.filesystem_id, harness.root_id),
                record: StateRecord::Inode(root_record(harness.root_id, 3, harness.limits)),
            },
        ]),
        CommitOutcome::MalformedRequest(crate::MalformedCommit::DirectoryCookieAllocation)
    ));
}

#[test]
fn zero_link_pin_sets_require_an_orphan_until_final_retirement() {
    let mut harness = Harness::new();
    let name = EntryName::new(b"file".to_vec(), harness.limits).unwrap();
    assert!(matches!(
        harness.commit(vec![
            StateChange::Delete(RecordKey::DirectoryEntry(
                harness.filesystem_id,
                harness.root_id,
                name,
            )),
            StateChange::Replace {
                key: RecordKey::Inode(harness.filesystem_id, harness.file_id),
                record: StateRecord::Inode(file_record(
                    harness.file_id,
                    QidPath::new(2).unwrap(),
                    harness.content_file_id,
                    0,
                    2,
                    None,
                    identity_times(1),
                    harness.limits,
                )),
            },
            StateChange::Replace {
                key: RecordKey::Inode(harness.filesystem_id, harness.root_id),
                record: StateRecord::Inode(root_record(harness.root_id, 2, harness.limits)),
            },
        ]),
        CommitOutcome::MalformedRequest(crate::MalformedCommit::InvalidRecord(_))
    ));
    assert!(matches!(
        harness.read(ReadQuery::Inode(harness.file_id)),
        Some(StateRecord::Inode(inode)) if inode.link_count() == 1
    ));
}

#[test]
fn managed_generations_cannot_be_replaced_without_exact_advancement() {
    let mut harness = Harness::new();
    let stale = file_record(
        harness.file_id,
        QidPath::new(2).unwrap(),
        harness.content_file_id,
        1,
        1,
        None,
        identity_times(9),
        harness.limits,
    );
    assert!(matches!(
        harness.commit(vec![StateChange::Replace {
            key: RecordKey::Inode(harness.filesystem_id, harness.file_id),
            record: StateRecord::Inode(stale),
        }]),
        CommitOutcome::MalformedRequest(crate::MalformedCommit::NonMonotonicTransition)
    ));

    let name = EntryName::new(b"missing-generation".to_vec(), harness.limits).unwrap();
    assert!(matches!(
        harness.commit(vec![
            StateChange::Insert {
                key: RecordKey::DirectoryEntry(
                    harness.filesystem_id,
                    harness.root_id,
                    name.clone(),
                ),
                record: StateRecord::DirectoryEntry(
                    DirectoryEntryRecord::new(
                        harness.root_id,
                        name,
                        DirectoryCookie::new(3),
                        harness.file_id,
                        RecordRevision::new(1).unwrap(),
                    )
                    .unwrap(),
                ),
            },
            StateChange::AdvanceDirectoryCookie {
                count: core::num::NonZeroU64::new(1).unwrap(),
            },
        ]),
        CommitOutcome::MalformedRequest(crate::MalformedCommit::NamespaceGeneration)
    ));
}

#[test]
fn incomplete_xattr_staging_cannot_be_published() {
    let mut harness = Harness::new();
    let staging_id = XattrStagingId::from_u128(60);
    let name = XattrName::new(b"user.incomplete".to_vec(), harness.limits).unwrap();
    let staging = XattrStagingRecord::new(
        staging_id,
        harness.file_id,
        name.clone(),
        5,
        XattrValue::new(b"ab".to_vec(), harness.limits).unwrap(),
        RecordRevision::new(1).unwrap(),
        harness.limits,
    )
    .unwrap();
    assert!(matches!(
        harness.commit(vec![StateChange::Insert {
            key: RecordKey::XattrStaging(harness.filesystem_id, staging_id),
            record: StateRecord::XattrStaging(staging),
        }]),
        CommitOutcome::Committed(_)
    ));
    assert!(matches!(
        harness.commit(vec![StateChange::PublishXattrStaging(
            crate::PublishXattrStaging {
                staging_id,
                inode_id: harness.file_id,
                name: name.clone(),
            },
        )]),
        CommitOutcome::MalformedRequest(crate::MalformedCommit::InvalidXattrStaging)
    ));
    assert!(matches!(
        harness.read(ReadQuery::XattrStaging(staging_id)),
        Some(StateRecord::XattrStaging(_))
    ));
    assert!(
        harness
            .read(ReadQuery::Xattr {
                inode_id: harness.file_id,
                name,
            })
            .is_none()
    );
}
