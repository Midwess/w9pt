//! Reusable semantic conformance checks for independently opened adapters.

use core::{fmt, future::Future};

use crate::{
    AcquireLeaseOutcome, AcquireWriterLease, ChangeCursor, ChangePoll, ChangePollOutcome,
    ClientIncarnationId, CommitOutcome, CommitRequest, DataGeneration, DirectoryCookie,
    DirectoryEntryRecord, DirectoryGeneration, EntryName, FilesystemId, FilesystemRecord,
    FilesystemStateStore, GroupId, InodeData, InodeGeneration, InodeId, InodeRecord, InodeTimes,
    LeaseDuration, LeaseId, LeaseOperationId, LeaseRejection, LockGeneration, LockId, LockKind,
    LockOwner, LockRange, LockRecord, MutationContext, MutationResult, MutationResultKind,
    MutationRetention, OpenAccess, OpenId, OpenPinRecord, OpenRecord, OrphanRecord, Precondition,
    PrincipalId, PublishContent, PublishXattrStaging, ReadBatch, ReadConsistency, ReadOutcome,
    ReadQuery, ReadResult, RecordKey, RecordRevision, RecordScan, ReleaseLeaseOutcome,
    ReleaseWriterLease, RenewLeaseOutcome, RenewWriterLease, ResultFormatVersion, ScanBounds,
    ScanResume, StateChange, StateLimitValues, StateLimits, StateRecord, StateRevision,
    SymlinkTarget, UnixTimestamp, WriterFence, WriterIncarnationId, WriterScopeId, WriterTopology,
    XattrName, XattrRecord, XattrStagingId, XattrStagingRecord, XattrValue,
};

/// Adapter-owned controls required for deterministic time and failure boundaries.
pub trait StateStoreConformanceHarness: Send + Sync {
    /// Independently opened cache-free store client.
    type Store: FilesystemStateStore;
    /// Harness-control failure.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Opens another client over the same fresh test authority.
    fn open_client(&self) -> Self::Store;

    /// Advances the adapter-authoritative test clock.
    fn advance_time(&self, ticks: u64) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Injects one deterministic commit failure at the requested boundary.
    fn inject_commit_failure(
        &self,
        timing: super::CommitFailureTiming,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Compacts retained change history while preserving the latest cursor boundary.
    fn compact_change_history(&self) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Opens one isolated empty authority using an explicit limit configuration.
    fn open_isolated_with_limits(
        &self,
        limits: StateLimits,
    ) -> impl Future<Output = Result<Self::Store, Self::Error>> + Send;
}

/// Runs semantic and failure-boundary checks against one fresh adapter harness.
pub async fn check_state_store_conformance<H>(harness: &H) -> Result<(), StateStoreConformanceError>
where
    H: StateStoreConformanceHarness,
{
    let writer = harness.open_client();
    let observer = harness.open_client();
    let contract = writer.contract();
    let limits = contract.limits();
    if limits.max_changes() < 4
        || limits.max_change_keys() < 5
        || limits.max_xattrs_per_request() < 2
    {
        return Err(StateStoreConformanceError::assertion(
            "limits cannot express required atomic filesystem transitions",
        ));
    }

    let filesystem_id = FilesystemId::from_u128(1);
    let root_id = InodeId::from_u128(2);
    let scope = match contract.writer_topology() {
        WriterTopology::SerializableMultiWriter => WriterScopeId::from_u128(3),
        WriterTopology::SingleFencedWriter => WriterScopeId::FILESYSTEM,
    };
    let acquire = AcquireWriterLease::new(
        filesystem_id,
        LeaseOperationId::from_u128(4),
        scope,
        WriterIncarnationId::from_u128(5),
        LeaseId::from_u128(6),
        LeaseDuration::new(1).expect("one is a valid lease duration"),
        limits,
    )
    .map_err(|error| StateStoreConformanceError::adapter("build lease request", error))?;
    let grant = match writer
        .acquire_writer_lease(acquire)
        .await
        .map_err(|error| StateStoreConformanceError::adapter("acquire lease", error))?
    {
        AcquireLeaseOutcome::Granted(grant) => grant,
        outcome => {
            return Err(StateStoreConformanceError::unexpected(
                "acquire lease",
                format!("{outcome:?}"),
            ));
        }
    };
    if !matches!(
        writer
            .acquire_writer_lease(acquire)
            .await
            .map_err(|error| StateStoreConformanceError::adapter("acquire replay", error))?,
        AcquireLeaseOutcome::AlreadyApplied(replayed) if replayed == grant
    ) {
        return Err(StateStoreConformanceError::assertion(
            "acquire replay did not return the retained grant",
        ));
    }
    let mismatched_acquire = AcquireWriterLease::new(
        filesystem_id,
        acquire.operation_id(),
        scope,
        acquire.holder(),
        LeaseId::from_u128(60),
        acquire.duration(),
        limits,
    )
    .map_err(|error| StateStoreConformanceError::adapter("build acquire mismatch", error))?;
    if !matches!(
        writer
            .acquire_writer_lease(mismatched_acquire)
            .await
            .map_err(|error| StateStoreConformanceError::adapter("acquire mismatch", error))?,
        AcquireLeaseOutcome::Rejected(LeaseRejection::OperationMismatch)
    ) {
        return Err(StateStoreConformanceError::assertion(
            "acquire operation identity mismatch was not rejected",
        ));
    }
    let renew = RenewWriterLease::new(
        filesystem_id,
        LeaseOperationId::from_u128(61),
        grant.fence,
        LeaseDuration::new(1).expect("one is a valid lease duration"),
        limits,
    )
    .map_err(|error| StateStoreConformanceError::adapter("build renew", error))?;
    let renewed = match writer
        .renew_writer_lease(renew)
        .await
        .map_err(|error| StateStoreConformanceError::adapter("renew lease", error))?
    {
        RenewLeaseOutcome::Renewed(renewed) => renewed,
        outcome => {
            return Err(StateStoreConformanceError::unexpected(
                "renew lease",
                format!("{outcome:?}"),
            ));
        }
    };
    if renewed.fence != grant.fence
        || !matches!(
            writer
                .renew_writer_lease(renew)
                .await
                .map_err(|error| StateStoreConformanceError::adapter("renew replay", error))?,
            RenewLeaseOutcome::AlreadyApplied(replayed) if replayed == renewed
        )
    {
        return Err(StateStoreConformanceError::assertion(
            "renewal changed the fence or failed exact replay",
        ));
    }
    let mismatched_renew = RenewWriterLease::new(
        filesystem_id,
        renew.operation_id(),
        WriterFence {
            lease_id: LeaseId::from_u128(62),
            ..grant.fence
        },
        renew.duration(),
        limits,
    )
    .map_err(|error| StateStoreConformanceError::adapter("build renew mismatch", error))?;
    if !matches!(
        writer
            .renew_writer_lease(mismatched_renew)
            .await
            .map_err(|error| StateStoreConformanceError::adapter("renew mismatch", error))?,
        RenewLeaseOutcome::Rejected(LeaseRejection::OperationMismatch)
    ) {
        return Err(StateStoreConformanceError::assertion(
            "renew operation identity mismatch was not rejected",
        ));
    }

    let timestamp = UnixTimestamp::new(0, 0).expect("zero timestamp is valid");
    let times = InodeTimes {
        accessed: timestamp,
        modified: timestamp,
        changed: timestamp,
        created: timestamp,
    };
    let root = InodeRecord::new(
        root_id,
        RecordRevision::new(1).expect("one is a valid revision"),
        0o755,
        PrincipalId::new(b"r".to_vec(), limits)
            .map_err(|error| StateStoreConformanceError::adapter("build principal", error))?,
        GroupId::new(b"g".to_vec(), limits)
            .map_err(|error| StateStoreConformanceError::adapter("build group", error))?,
        times,
        0,
        1,
        InodeGeneration::new(1).expect("one is a valid generation"),
        InodeData::Directory {
            generation: DirectoryGeneration::new(1).expect("one is a valid generation"),
        },
    )
    .map_err(|error| StateStoreConformanceError::adapter("build root inode", error))?;
    let filesystem = FilesystemRecord::new(
        filesystem_id,
        StateRevision::new(1).expect("one is a valid revision"),
        RecordRevision::new(1).expect("one is a valid revision"),
        root_id,
        DirectoryCookie::new(1),
        1,
    )
    .map_err(|error| StateStoreConformanceError::adapter("build filesystem", error))?;
    let mutation = MutationContext::new(
        w9pt_storage::MutationId::from_u128(7),
        crate::RequestFingerprint::blake3(b"conformance bootstrap"),
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
        terminal_result(b"r", limits)?,
        limits,
    )
    .map_err(|error| StateStoreConformanceError::adapter("build bootstrap commit", error))?;
    let committed_revision = match writer
        .commit(request.clone())
        .await
        .map_err(|error| StateStoreConformanceError::adapter("bootstrap commit", error))?
    {
        CommitOutcome::Committed(committed) => committed.revision,
        outcome => {
            return Err(StateStoreConformanceError::unexpected(
                "bootstrap commit",
                format!("{outcome:?}"),
            ));
        }
    };

    let failure_mutation = MutationContext::new(
        w9pt_storage::MutationId::from_u128(70),
        crate::RequestFingerprint::blake3(b"failure-boundary"),
        ClientIncarnationId::from_u128(71),
        MutationRetention::new(100),
    );
    let failure_request = CommitRequest::new(
        filesystem_id,
        failure_mutation,
        grant.fence,
        vec![],
        vec![StateChange::BumpInodeGeneration(root_id)],
        terminal_result(b"r", limits)?,
        limits,
    )
    .map_err(|error| StateStoreConformanceError::adapter("build failure commit", error))?;
    harness
        .inject_commit_failure(super::CommitFailureTiming::BeforePublication)
        .await
        .map_err(|error| StateStoreConformanceError::adapter("inject before failure", error))?;
    if writer.commit(failure_request.clone()).await.is_ok() {
        return Err(StateStoreConformanceError::assertion(
            "before-publication failure did not fail definitively",
        ));
    }
    if read_inode_generation(&observer, filesystem_id, root_id, limits)
        .await?
        .get()
        != 1
    {
        return Err(StateStoreConformanceError::assertion(
            "before-publication failure exposed partial inode state",
        ));
    }
    harness
        .inject_commit_failure(super::CommitFailureTiming::AfterPublication)
        .await
        .map_err(|error| StateStoreConformanceError::adapter("inject after failure", error))?;
    if !matches!(
        writer
            .commit(failure_request.clone())
            .await
            .map_err(|error| StateStoreConformanceError::adapter("ambiguous commit", error))?,
        CommitOutcome::Ambiguous(_)
    ) {
        return Err(StateStoreConformanceError::assertion(
            "after-publication failure did not report ambiguity",
        ));
    }
    if !matches!(
        writer
            .commit(failure_request)
            .await
            .map_err(|error| StateStoreConformanceError::adapter("ambiguous replay", error))?,
        CommitOutcome::AlreadyCommitted(_)
    ) {
        return Err(StateStoreConformanceError::assertion(
            "ambiguous commit was not resolved by exact replay",
        ));
    }
    if read_inode_generation(&observer, filesystem_id, root_id, limits)
        .await?
        .get()
        != 2
    {
        return Err(StateStoreConformanceError::assertion(
            "ambiguous commit was not visible exactly once to an independent client",
        ));
    }

    let read = ReadBatch::new(
        filesystem_id,
        ReadConsistency::AtLeast(committed_revision),
        vec![ReadQuery::Filesystem, ReadQuery::Inode(root_id)],
        limits,
    )
    .map_err(|error| StateStoreConformanceError::adapter("build read", error))?;
    let snapshot = match observer
        .read(read)
        .await
        .map_err(|error| StateStoreConformanceError::adapter("independent read", error))?
    {
        ReadOutcome::Snapshot(snapshot) => snapshot,
        outcome => {
            return Err(StateStoreConformanceError::unexpected(
                "independent read",
                format!("{outcome:?}"),
            ));
        }
    };
    if snapshot.revision() < committed_revision
        || !snapshot.results().iter().all(|result| {
            matches!(
                result,
                ReadResult::Point {
                    record: Some(_),
                    ..
                }
            )
        })
    {
        return Err(StateStoreConformanceError::assertion(
            "independent client did not observe one complete commit",
        ));
    }

    check_record_set_semantics(&writer, filesystem_id, root_id, grant.fence, times, limits).await?;
    check_independent_client_commits(
        &writer,
        &observer,
        filesystem_id,
        root_id,
        grant.fence,
        limits,
    )
    .await?;

    if !matches!(
        writer
            .commit(request.clone())
            .await
            .map_err(|error| StateStoreConformanceError::adapter("mutation replay", error))?,
        CommitOutcome::AlreadyCommitted(_)
    ) {
        return Err(StateStoreConformanceError::assertion(
            "exact mutation replay was not retained",
        ));
    }
    let mismatch = CommitRequest::new(
        filesystem_id,
        MutationContext::new(
            mutation.mutation_id,
            crate::RequestFingerprint::blake3(b"different request"),
            mutation.client_incarnation,
            mutation.retention,
        ),
        grant.fence,
        request.preconditions().to_vec(),
        request.changes().to_vec(),
        terminal_result(b"r", limits)?,
        limits,
    )
    .map_err(|error| StateStoreConformanceError::adapter("build mismatch", error))?;
    if !matches!(
        writer
            .commit(mismatch)
            .await
            .map_err(|error| StateStoreConformanceError::adapter("mutation mismatch", error))?,
        CommitOutcome::MutationMismatch(_)
    ) {
        return Err(StateStoreConformanceError::assertion(
            "mutation identity reuse was not rejected",
        ));
    }

    let xattr_name = XattrName::new(b"x".to_vec(), limits)
        .map_err(|error| StateStoreConformanceError::adapter("build xattr name", error))?;
    let xattr_key = RecordKey::Xattr(filesystem_id, root_id, xattr_name.clone());
    let atomic_request = CommitRequest::new(
        filesystem_id,
        MutationContext::new(
            w9pt_storage::MutationId::from_u128(9),
            crate::RequestFingerprint::blake3(b"atomic failure"),
            ClientIncarnationId::from_u128(10),
            MutationRetention::new(100),
        ),
        grant.fence,
        vec![],
        vec![
            StateChange::Insert {
                key: xattr_key.clone(),
                record: StateRecord::Xattr(XattrRecord::new(
                    root_id,
                    xattr_name,
                    XattrValue::new(b"v".to_vec(), limits).map_err(|error| {
                        StateStoreConformanceError::adapter("build xattr value", error)
                    })?,
                    RecordRevision::new(1).expect("one is a valid revision"),
                )),
            },
            StateChange::Delete(RecordKey::Inode(filesystem_id, InodeId::from_u128(99))),
        ],
        terminal_result(b"r", limits)?,
        limits,
    )
    .map_err(|error| StateStoreConformanceError::adapter("build atomicity request", error))?;
    if !matches!(
        writer
            .commit(atomic_request)
            .await
            .map_err(|error| StateStoreConformanceError::adapter("atomicity commit", error))?,
        CommitOutcome::Conflict(_)
    ) {
        return Err(StateStoreConformanceError::assertion(
            "late conflict did not reject complete commit",
        ));
    }
    let absent = ReadBatch::new(
        filesystem_id,
        ReadConsistency::LatestLinearizable,
        vec![ReadQuery::Xattr {
            inode_id: root_id,
            name: match &xattr_key {
                RecordKey::Xattr(_, _, name) => name.clone(),
                _ => unreachable!(),
            },
        }],
        limits,
    )
    .map_err(|error| StateStoreConformanceError::adapter("build atomicity read", error))?;
    if !matches!(
        observer
            .read(absent)
            .await
            .map_err(|error| StateStoreConformanceError::adapter("atomicity read", error))?,
        ReadOutcome::Snapshot(snapshot)
            if matches!(&snapshot.results()[0], ReadResult::Point { record: None, .. })
    ) {
        return Err(StateStoreConformanceError::assertion(
            "partial state escaped a rejected commit",
        ));
    }

    let stale_fence = WriterFence {
        fencing_token: grant
            .fence
            .fencing_token
            .checked_next()
            .map_err(|error| StateStoreConformanceError::adapter("build stale fence", error))?,
        ..grant.fence
    };
    let stale_request = CommitRequest::new(
        filesystem_id,
        MutationContext::new(
            w9pt_storage::MutationId::from_u128(11),
            crate::RequestFingerprint::blake3(b"stale fence"),
            ClientIncarnationId::from_u128(12),
            MutationRetention::new(100),
        ),
        stale_fence,
        vec![],
        vec![StateChange::BumpInodeGeneration(root_id)],
        terminal_result(b"r", limits)?,
        limits,
    )
    .map_err(|error| StateStoreConformanceError::adapter("build stale commit", error))?;
    if !matches!(
        writer
            .commit(stale_request)
            .await
            .map_err(|error| StateStoreConformanceError::adapter("stale commit", error))?,
        CommitOutcome::StaleFence
    ) {
        return Err(StateStoreConformanceError::assertion(
            "stale fencing token was not rejected",
        ));
    }

    let poll = ChangePoll::new(
        filesystem_id,
        ChangeCursor::after(StateRevision::new(1).expect("one is a valid revision")),
        limits.max_change_history_commits().min(8),
        limits.max_change_keys(),
        limits,
    )
    .map_err(|error| StateStoreConformanceError::adapter("build change poll", error))?;
    let changes = observer
        .poll_changes(poll)
        .await
        .map_err(|error| StateStoreConformanceError::adapter("change poll", error))?;
    let ChangePollOutcome::Changes(changes) = changes else {
        return Err(StateStoreConformanceError::unexpected(
            "change poll",
            format!("{changes:?}"),
        ));
    };
    if changes.events().is_empty() {
        return Err(StateStoreConformanceError::assertion(
            "change poll omitted committed revisions",
        ));
    }
    if !changes.events().iter().any(|event| {
        matches!(event.origin(), crate::ChangeOrigin::Mutation(_))
            && event
                .keys()
                .iter()
                .any(|key| matches!(key, RecordKey::Mutation(_, _)))
    }) {
        return Err(StateStoreConformanceError::assertion(
            "commit event omitted mutation origin or ledger key",
        ));
    }
    if let Some(large_event) = changes.events().iter().find(|event| event.keys().len() > 1) {
        let prior = StateRevision::new(large_event.revision().get() - 1)
            .expect("event revision follows revision one");
        let too_small = ChangePoll::new(filesystem_id, ChangeCursor::after(prior), 1, 1, limits)
            .map_err(|error| StateStoreConformanceError::adapter("build small poll", error))?;
        if !matches!(
            observer
                .poll_changes(too_small)
                .await
                .map_err(|error| StateStoreConformanceError::adapter("small poll", error))?,
            ChangePollOutcome::PollBoundTooSmall { .. }
        ) {
            return Err(StateStoreConformanceError::assertion(
                "change poll split or skipped an oversized whole event",
            ));
        }
    }
    let empty_poll = ChangePoll::new(
        filesystem_id,
        ChangeCursor::after(changes.current_revision()),
        1,
        limits.max_change_keys(),
        limits,
    )
    .map_err(|error| StateStoreConformanceError::adapter("build empty poll", error))?;
    if !matches!(
        observer
            .poll_changes(empty_poll)
            .await
            .map_err(|error| StateStoreConformanceError::adapter("empty poll", error))?,
        ChangePollOutcome::Changes(batch) if batch.events().is_empty()
    ) {
        return Err(StateStoreConformanceError::assertion(
            "empty change poll did not return a stable empty batch",
        ));
    }
    let future_revision = changes
        .current_revision()
        .checked_next()
        .map_err(|error| StateStoreConformanceError::adapter("future poll revision", error))?;
    let future_poll = ChangePoll::new(
        filesystem_id,
        ChangeCursor::after(future_revision),
        1,
        1,
        limits,
    )
    .map_err(|error| StateStoreConformanceError::adapter("build future poll", error))?;
    if !matches!(
        observer
            .poll_changes(future_poll)
            .await
            .map_err(|error| StateStoreConformanceError::adapter("future poll", error))?,
        ChangePollOutcome::RevisionUnavailable { .. }
    ) {
        return Err(StateStoreConformanceError::assertion(
            "future change cursor was not returned as a semantic revision outcome",
        ));
    }

    let scan = ScanBounds::new(1, limits.max_scan_bytes(), limits)
        .map_err(|error| StateStoreConformanceError::adapter("build bounded scan", error))?;
    let scan_request = ReadBatch::new(
        filesystem_id,
        ReadConsistency::LatestLinearizable,
        vec![ReadQuery::Scan(RecordScan::Inodes {
            after: None,
            bounds: scan,
        })],
        limits,
    )
    .map_err(|error| StateStoreConformanceError::adapter("build scan read", error))?;
    if !matches!(
        observer
            .read(scan_request)
            .await
            .map_err(|error| StateStoreConformanceError::adapter("bounded scan", error))?,
        ReadOutcome::Snapshot(snapshot)
            if matches!(&snapshot.results()[0], ReadResult::Scan(page) if page.records().len() <= 1)
    ) {
        return Err(StateStoreConformanceError::assertion(
            "adapter did not honor scan item bound",
        ));
    }
    let byte_scan = ScanBounds::new(1, 1, limits)
        .map_err(|error| StateStoreConformanceError::adapter("build byte scan", error))?;
    let byte_scan_request = ReadBatch::new(
        filesystem_id,
        ReadConsistency::LatestLinearizable,
        vec![ReadQuery::Scan(RecordScan::Inodes {
            after: None,
            bounds: byte_scan,
        })],
        limits,
    )
    .map_err(|error| StateStoreConformanceError::adapter("build byte scan read", error))?;
    if !matches!(
        observer
            .read(byte_scan_request)
            .await
            .map_err(|error| StateStoreConformanceError::adapter("byte bounded scan", error))?,
        ReadOutcome::ScanBoundTooSmall { .. }
    ) {
        return Err(StateStoreConformanceError::assertion(
            "adapter did not enforce scan byte bounds before materialization",
        ));
    }

    check_receiver_limits(&writer, filesystem_id, root_id, grant.fence, scope, limits).await?;
    check_isolated_limit_authorities(harness).await?;

    let release =
        ReleaseWriterLease::new(filesystem_id, LeaseOperationId::from_u128(80), grant.fence);
    if !matches!(
        writer
            .release_writer_lease(release)
            .await
            .map_err(|error| StateStoreConformanceError::adapter("release lease", error))?,
        ReleaseLeaseOutcome::Released
    ) || !matches!(
        writer
            .release_writer_lease(release)
            .await
            .map_err(|error| StateStoreConformanceError::adapter("release replay", error))?,
        ReleaseLeaseOutcome::AlreadyApplied
    ) {
        return Err(StateStoreConformanceError::assertion(
            "lease release or its exact replay failed",
        ));
    }
    let mismatched_release = ReleaseWriterLease::new(
        filesystem_id,
        release.operation_id(),
        WriterFence {
            lease_id: LeaseId::from_u128(87),
            ..grant.fence
        },
    );
    if !matches!(
        writer
            .release_writer_lease(mismatched_release)
            .await
            .map_err(|error| StateStoreConformanceError::adapter("release mismatch", error))?,
        ReleaseLeaseOutcome::Rejected(LeaseRejection::OperationMismatch)
    ) {
        return Err(StateStoreConformanceError::assertion(
            "release operation identity mismatch was not rejected",
        ));
    }
    let after_release = AcquireWriterLease::new(
        filesystem_id,
        LeaseOperationId::from_u128(81),
        scope,
        WriterIncarnationId::from_u128(82),
        LeaseId::from_u128(83),
        LeaseDuration::new(1).expect("one is a valid lease duration"),
        limits,
    )
    .map_err(|error| StateStoreConformanceError::adapter("build post-release acquire", error))?;
    let released_grant = match writer
        .acquire_writer_lease(after_release)
        .await
        .map_err(|error| StateStoreConformanceError::adapter("post-release acquire", error))?
    {
        AcquireLeaseOutcome::Granted(grant) => grant,
        outcome => {
            return Err(StateStoreConformanceError::unexpected(
                "post-release acquire",
                format!("{outcome:?}"),
            ));
        }
    };
    if released_grant.fence.fencing_token <= grant.fence.fencing_token {
        return Err(StateStoreConformanceError::assertion(
            "release reset or reused a fencing token",
        ));
    }
    harness
        .advance_time(1)
        .await
        .map_err(|error| StateStoreConformanceError::adapter("advance lease time", error))?;
    let takeover = AcquireWriterLease::new(
        filesystem_id,
        LeaseOperationId::from_u128(84),
        scope,
        WriterIncarnationId::from_u128(85),
        LeaseId::from_u128(86),
        LeaseDuration::new(1).expect("one is a valid lease duration"),
        limits,
    )
    .map_err(|error| StateStoreConformanceError::adapter("build expiry takeover", error))?;
    let takeover = match writer
        .acquire_writer_lease(takeover)
        .await
        .map_err(|error| StateStoreConformanceError::adapter("expiry takeover", error))?
    {
        AcquireLeaseOutcome::Granted(grant) => grant,
        outcome => {
            return Err(StateStoreConformanceError::unexpected(
                "expiry takeover",
                format!("{outcome:?}"),
            ));
        }
    };
    if takeover.fence.fencing_token <= released_grant.fence.fencing_token {
        return Err(StateStoreConformanceError::assertion(
            "expiry takeover did not advance the fencing token",
        ));
    }
    harness
        .compact_change_history()
        .await
        .map_err(|error| StateStoreConformanceError::adapter("compact change history", error))?;
    let compacted_poll = ChangePoll::new(
        filesystem_id,
        ChangeCursor::after(StateRevision::new(1).expect("one is nonzero")),
        1,
        limits.max_change_keys(),
        limits,
    )
    .map_err(|error| StateStoreConformanceError::adapter("build compacted poll", error))?;
    if !matches!(
        observer
            .poll_changes(compacted_poll)
            .await
            .map_err(|error| StateStoreConformanceError::adapter("compacted poll", error))?,
        ChangePollOutcome::RevisionCompacted { .. }
    ) {
        return Err(StateStoreConformanceError::assertion(
            "compacted revision did not force authoritative cache reload",
        ));
    }
    Ok(())
}

async fn check_isolated_limit_authorities<H: StateStoreConformanceHarness>(
    harness: &H,
) -> Result<(), StateStoreConformanceError> {
    let filesystem_id = FilesystemId::from_u128(600);
    let fence = WriterFence {
        scope: WriterScopeId::FILESYSTEM,
        holder: WriterIncarnationId::from_u128(601),
        lease_id: LeaseId::from_u128(602),
        fencing_token: crate::FencingToken::new(1).expect("one is nonzero"),
    };

    let strict_values = StateLimitValues {
        max_locks_per_request: 1,
        ..StateLimitValues::default()
    };
    let strict = StateLimits::new(strict_values)
        .map_err(|error| StateStoreConformanceError::adapter("strict lock limits", error))?;
    let store = harness
        .open_isolated_with_limits(strict)
        .await
        .map_err(|error| StateStoreConformanceError::adapter("open lock-limit store", error))?;
    let mut loose_values = strict_values;
    loose_values.max_locks_per_request = 2;
    let loose = StateLimits::new(loose_values)
        .map_err(|error| StateStoreConformanceError::adapter("loose lock limits", error))?;
    let request = conformance_request(
        filesystem_id,
        603,
        fence,
        vec![],
        vec![
            StateChange::Delete(RecordKey::Lock(
                filesystem_id,
                InodeId::from_u128(1),
                LockId::from_u128(1),
            )),
            StateChange::Delete(RecordKey::Lock(
                filesystem_id,
                InodeId::from_u128(2),
                LockId::from_u128(2),
            )),
        ],
        loose,
    )?;
    expect_commit_limit(
        &store,
        request,
        crate::StateLimitKind::Locks,
        "adapter accepted too many lock changes",
    )
    .await?;

    let strict_values = StateLimitValues {
        max_open_pins_per_request: 1,
        ..StateLimitValues::default()
    };
    let strict = StateLimits::new(strict_values)
        .map_err(|error| StateStoreConformanceError::adapter("strict pin limits", error))?;
    let store = harness
        .open_isolated_with_limits(strict)
        .await
        .map_err(|error| StateStoreConformanceError::adapter("open pin-limit store", error))?;
    let mut loose_values = strict_values;
    loose_values.max_open_pins_per_request = 2;
    let loose = StateLimits::new(loose_values)
        .map_err(|error| StateStoreConformanceError::adapter("loose pin limits", error))?;
    let request = conformance_request(
        filesystem_id,
        604,
        fence,
        vec![],
        vec![
            StateChange::Delete(RecordKey::OpenPin(
                filesystem_id,
                InodeId::from_u128(1),
                OpenId::from_u128(1),
            )),
            StateChange::Delete(RecordKey::OpenPin(
                filesystem_id,
                InodeId::from_u128(2),
                OpenId::from_u128(2),
            )),
        ],
        loose,
    )?;
    expect_commit_limit(
        &store,
        request,
        crate::StateLimitKind::OpenPins,
        "adapter accepted too many open-pin changes",
    )
    .await?;

    let strict_values = StateLimitValues {
        max_xattrs_per_request: 2,
        ..StateLimitValues::default()
    };
    let strict = StateLimits::new(strict_values)
        .map_err(|error| StateStoreConformanceError::adapter("strict xattr limits", error))?;
    let store = harness
        .open_isolated_with_limits(strict)
        .await
        .map_err(|error| StateStoreConformanceError::adapter("open xattr-limit store", error))?;
    let mut loose_values = strict_values;
    loose_values.max_xattrs_per_request = 3;
    let loose = StateLimits::new(loose_values)
        .map_err(|error| StateStoreConformanceError::adapter("loose xattr limits", error))?;
    let name = XattrName::new(b"x".to_vec(), loose)
        .map_err(|error| StateStoreConformanceError::adapter("xattr limit name", error))?;
    let request = conformance_request(
        filesystem_id,
        605,
        fence,
        vec![],
        vec![
            StateChange::Delete(RecordKey::Xattr(
                filesystem_id,
                InodeId::from_u128(1),
                name.clone(),
            )),
            StateChange::Delete(RecordKey::Xattr(
                filesystem_id,
                InodeId::from_u128(2),
                name.clone(),
            )),
            StateChange::Delete(RecordKey::Xattr(filesystem_id, InodeId::from_u128(3), name)),
        ],
        loose,
    )?;
    expect_commit_limit(
        &store,
        request,
        crate::StateLimitKind::Xattrs,
        "adapter accepted too many xattr changes",
    )
    .await?;

    let strict = StateLimits::new(StateLimitValues {
        max_changes: 4,
        max_change_keys: 5,
        ..StateLimitValues::default()
    })
    .map_err(|error| StateStoreConformanceError::adapter("strict change-key limits", error))?;
    let store = harness
        .open_isolated_with_limits(strict)
        .await
        .map_err(|error| StateStoreConformanceError::adapter("open change-key store", error))?;
    let loose = StateLimits::new(StateLimitValues {
        max_changes: 4,
        max_change_keys: 7,
        max_xattrs_per_request: 6,
        ..StateLimitValues::default()
    })
    .map_err(|error| StateStoreConformanceError::adapter("loose change-key limits", error))?;
    let name = XattrName::new(b"x".to_vec(), loose)
        .map_err(|error| StateStoreConformanceError::adapter("change-key xattr name", error))?;
    let changes = (0..3u128)
        .map(|index| {
            StateChange::PublishXattrStaging(PublishXattrStaging {
                staging_id: XattrStagingId::from_u128(610 + index),
                inode_id: InodeId::from_u128(620 + index),
                name: name.clone(),
            })
        })
        .collect();
    let request = conformance_request(filesystem_id, 606, fence, vec![], changes, loose)?;
    expect_commit_limit(
        &store,
        request,
        crate::StateLimitKind::ChangeKeys,
        "adapter accepted too many changed-record keys",
    )
    .await?;

    check_isolated_lease_history(harness).await?;
    check_isolated_transaction_bytes(harness).await?;
    Ok(())
}

async fn check_isolated_lease_history<H: StateStoreConformanceHarness>(
    harness: &H,
) -> Result<(), StateStoreConformanceError> {
    let limits = StateLimits::new(StateLimitValues {
        max_lease_operation_history: 2,
        ..StateLimitValues::default()
    })
    .map_err(|error| StateStoreConformanceError::adapter("lease-history limits", error))?;
    let store = harness
        .open_isolated_with_limits(limits)
        .await
        .map_err(|error| StateStoreConformanceError::adapter("open lease-history store", error))?;
    let filesystem_id = FilesystemId::from_u128(700);
    let scope = match store.contract().writer_topology() {
        WriterTopology::SerializableMultiWriter => WriterScopeId::from_u128(701),
        WriterTopology::SingleFencedWriter => WriterScopeId::FILESYSTEM,
    };
    let first = AcquireWriterLease::new(
        filesystem_id,
        LeaseOperationId::from_u128(702),
        scope,
        WriterIncarnationId::from_u128(703),
        LeaseId::from_u128(704),
        LeaseDuration::new(1).expect("one is nonzero"),
        limits,
    )
    .map_err(|error| StateStoreConformanceError::adapter("first history lease", error))?;
    if !matches!(
        store
            .acquire_writer_lease(first)
            .await
            .map_err(|error| StateStoreConformanceError::adapter("first history grant", error))?,
        AcquireLeaseOutcome::Granted(_)
    ) {
        return Err(StateStoreConformanceError::assertion(
            "lease-history authority did not grant its first operation",
        ));
    }
    let second = AcquireWriterLease::new(
        filesystem_id,
        LeaseOperationId::from_u128(705),
        scope,
        WriterIncarnationId::from_u128(706),
        LeaseId::from_u128(707),
        LeaseDuration::new(1).expect("one is nonzero"),
        limits,
    )
    .map_err(|error| StateStoreConformanceError::adapter("second history lease", error))?;
    if !matches!(
        store
            .acquire_writer_lease(second)
            .await
            .map_err(|error| StateStoreConformanceError::adapter("second history grant", error))?,
        AcquireLeaseOutcome::Rejected(LeaseRejection::Busy(_))
    ) {
        return Err(StateStoreConformanceError::assertion(
            "lease-history authority did not retain a rejected operation",
        ));
    }
    let third = AcquireWriterLease::new(
        filesystem_id,
        LeaseOperationId::from_u128(708),
        scope,
        WriterIncarnationId::from_u128(709),
        LeaseId::from_u128(710),
        LeaseDuration::new(1).expect("one is nonzero"),
        limits,
    )
    .map_err(|error| StateStoreConformanceError::adapter("third history lease", error))?;
    if !matches!(
        store
            .acquire_writer_lease(third)
            .await
            .map_err(|error| StateStoreConformanceError::adapter("history exhaustion", error))?,
        AcquireLeaseOutcome::Rejected(LeaseRejection::OperationHistoryFull)
    ) {
        return Err(StateStoreConformanceError::assertion(
            "lease operation history did not enforce its configured bound",
        ));
    }
    Ok(())
}

async fn check_isolated_transaction_bytes<H: StateStoreConformanceHarness>(
    harness: &H,
) -> Result<(), StateStoreConformanceError> {
    let limits = StateLimits::new(StateLimitValues {
        max_xattr_value_bytes: 400,
        max_mutation_result_bytes: 500,
        max_transaction_bytes: 1_000,
        ..StateLimitValues::default()
    })
    .map_err(|error| StateStoreConformanceError::adapter("transaction-byte limits", error))?;
    let store = harness
        .open_isolated_with_limits(limits)
        .await
        .map_err(|error| StateStoreConformanceError::adapter("open transaction store", error))?;
    let filesystem_id = FilesystemId::from_u128(720);
    let root_id = InodeId::from_u128(721);
    let scope = match store.contract().writer_topology() {
        WriterTopology::SerializableMultiWriter => WriterScopeId::from_u128(722),
        WriterTopology::SingleFencedWriter => WriterScopeId::FILESYSTEM,
    };
    let acquire = AcquireWriterLease::new(
        filesystem_id,
        LeaseOperationId::from_u128(723),
        scope,
        WriterIncarnationId::from_u128(724),
        LeaseId::from_u128(725),
        LeaseDuration::new(1).expect("one is nonzero"),
        limits,
    )
    .map_err(|error| StateStoreConformanceError::adapter("transaction lease", error))?;
    let grant = match store
        .acquire_writer_lease(acquire)
        .await
        .map_err(|error| StateStoreConformanceError::adapter("transaction grant", error))?
    {
        AcquireLeaseOutcome::Granted(grant) => grant,
        outcome => {
            return Err(StateStoreConformanceError::unexpected(
                "transaction grant",
                format!("{outcome:?}"),
            ));
        }
    };
    let times = InodeTimes {
        accessed: UnixTimestamp::new(0, 0).expect("valid timestamp"),
        modified: UnixTimestamp::new(0, 0).expect("valid timestamp"),
        changed: UnixTimestamp::new(0, 0).expect("valid timestamp"),
        created: UnixTimestamp::new(0, 0).expect("valid timestamp"),
    };
    let filesystem = FilesystemRecord::new(
        filesystem_id,
        StateRevision::new(1).expect("one is nonzero"),
        RecordRevision::new(1).expect("one is nonzero"),
        root_id,
        DirectoryCookie::new(1),
        1,
    )
    .map_err(|error| StateStoreConformanceError::adapter("transaction filesystem", error))?;
    let root = conformance_root(root_id, 1, 1, times, limits)?;
    require_committed(
        "transaction bootstrap",
        commit_changes(
            &store,
            filesystem_id,
            726,
            grant.fence,
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
            limits,
        )
        .await?,
    )?;
    let staging_id = XattrStagingId::from_u128(727);
    let name = XattrName::new(b"x".to_vec(), limits)
        .map_err(|error| StateStoreConformanceError::adapter("transaction xattr name", error))?;
    let staging = XattrStagingRecord::new(
        staging_id,
        root_id,
        name.clone(),
        400,
        XattrValue::new(vec![0; 400], limits)
            .map_err(|error| StateStoreConformanceError::adapter("transaction staging", error))?,
        RecordRevision::new(1).expect("one is nonzero"),
        limits,
    )
    .map_err(|error| StateStoreConformanceError::adapter("transaction staging record", error))?;
    require_committed(
        "transaction staging commit",
        commit_changes(
            &store,
            filesystem_id,
            728,
            grant.fence,
            vec![StateChange::Insert {
                key: RecordKey::XattrStaging(filesystem_id, staging_id),
                record: StateRecord::XattrStaging(staging),
            }],
            limits,
        )
        .await?,
    )?;
    let request = CommitRequest::new(
        filesystem_id,
        MutationContext::new(
            w9pt_storage::MutationId::from_u128(729),
            crate::RequestFingerprint::blake3(b"transaction-xattr"),
            ClientIncarnationId::from_u128(730),
            MutationRetention::new(100),
        ),
        grant.fence,
        vec![],
        vec![StateChange::PublishXattrStaging(PublishXattrStaging {
            staging_id,
            inode_id: root_id,
            name,
        })],
        MutationResult::new(
            MutationResultKind::new(1).expect("one is nonzero"),
            ResultFormatVersion::new(1).expect("one is nonzero"),
            vec![0; 300],
            limits,
        )
        .map_err(|error| StateStoreConformanceError::adapter("transaction result", error))?,
        limits,
    )
    .map_err(|error| StateStoreConformanceError::adapter("transaction publish request", error))?;
    expect_commit_limit(
        &store,
        request,
        crate::StateLimitKind::TransactionBytes,
        "adapter omitted authoritative staged xattr bytes from transaction accounting",
    )
    .await
}

async fn read_inode_generation<S: FilesystemStateStore>(
    store: &S,
    filesystem_id: FilesystemId,
    inode_id: InodeId,
    limits: StateLimits,
) -> Result<InodeGeneration, StateStoreConformanceError> {
    let request = ReadBatch::new(
        filesystem_id,
        ReadConsistency::LatestLinearizable,
        vec![ReadQuery::Inode(inode_id)],
        limits,
    )
    .map_err(|error| StateStoreConformanceError::adapter("build inode read", error))?;
    match store
        .read(request)
        .await
        .map_err(|error| StateStoreConformanceError::adapter("read inode generation", error))?
    {
        ReadOutcome::Snapshot(snapshot) => match &snapshot.results()[0] {
            ReadResult::Point {
                record: Some(record),
                ..
            } => match record.as_ref() {
                StateRecord::Inode(inode) => Ok(inode.inode_generation()),
                _ => Err(StateStoreConformanceError::assertion(
                    "inode read returned another record family",
                )),
            },
            _ => Err(StateStoreConformanceError::assertion(
                "inode read returned no record",
            )),
        },
        outcome => Err(StateStoreConformanceError::unexpected(
            "read inode generation",
            format!("{outcome:?}"),
        )),
    }
}

async fn check_independent_client_commits<S: FilesystemStateStore>(
    writer: &S,
    observer: &S,
    filesystem_id: FilesystemId,
    root_id: InodeId,
    fence: WriterFence,
    limits: StateLimits,
) -> Result<(), StateStoreConformanceError> {
    let observed = ReadBatch::new(
        filesystem_id,
        ReadConsistency::LatestLinearizable,
        vec![ReadQuery::Filesystem, ReadQuery::Inode(root_id)],
        limits,
    )
    .map_err(|error| StateStoreConformanceError::adapter("build concurrency read", error))?;
    let snapshot = match observer
        .read(observed)
        .await
        .map_err(|error| StateStoreConformanceError::adapter("concurrency read", error))?
    {
        ReadOutcome::Snapshot(snapshot) => snapshot,
        outcome => {
            return Err(StateStoreConformanceError::unexpected(
                "concurrency read",
                format!("{outcome:?}"),
            ));
        }
    };
    let filesystem_revision = match &snapshot.results()[0] {
        ReadResult::Point {
            record: Some(record),
            ..
        } => record.revision(),
        _ => {
            return Err(StateStoreConformanceError::assertion(
                "concurrency read omitted filesystem record",
            ));
        }
    };
    let root_generation = match &snapshot.results()[1] {
        ReadResult::Point {
            record: Some(record),
            ..
        } => match record.as_ref() {
            StateRecord::Inode(inode) => inode.inode_generation(),
            _ => {
                return Err(StateStoreConformanceError::assertion(
                    "concurrency read returned wrong root record",
                ));
            }
        },
        _ => {
            return Err(StateStoreConformanceError::assertion(
                "concurrency read omitted root inode",
            ));
        }
    };

    let first = conformance_request(
        filesystem_id,
        300,
        fence,
        vec![Precondition::InodeGeneration {
            inode_id: root_id,
            expected: root_generation,
        }],
        vec![StateChange::BumpInodeGeneration(root_id)],
        limits,
    )?;
    let conflicting = conformance_request(
        filesystem_id,
        301,
        fence,
        first.preconditions().to_vec(),
        first.changes().to_vec(),
        limits,
    )?;
    let (first_outcome, second_outcome) =
        join2(writer.commit(first), observer.commit(conflicting)).await;
    let first_outcome = first_outcome
        .map_err(|error| StateStoreConformanceError::adapter("first concurrent conflict", error))?;
    let second_outcome = second_outcome.map_err(|error| {
        StateStoreConformanceError::adapter("second concurrent conflict", error)
    })?;
    if !matches!(
        (&first_outcome, &second_outcome),
        (CommitOutcome::Committed(_), CommitOutcome::Conflict(_))
            | (CommitOutcome::Conflict(_), CommitOutcome::Committed(_))
    ) {
        return Err(StateStoreConformanceError::assertion(
            "concurrent conflicting commits did not serialize to one commit and one conflict",
        ));
    }

    let next_generation = root_generation
        .checked_next()
        .map_err(|error| StateStoreConformanceError::adapter("advance root generation", error))?;
    let disjoint_inode = conformance_request(
        filesystem_id,
        302,
        fence,
        vec![Precondition::InodeGeneration {
            inode_id: root_id,
            expected: next_generation,
        }],
        vec![StateChange::BumpInodeGeneration(root_id)],
        limits,
    )?;
    let disjoint_policy = conformance_request(
        filesystem_id,
        303,
        fence,
        vec![Precondition::RecordRevision {
            key: RecordKey::Filesystem(filesystem_id),
            expected: filesystem_revision,
        }],
        vec![StateChange::BumpFilesystemPolicyGeneration],
        limits,
    )?;
    let (inode_outcome, policy_outcome) = join2(
        writer.commit(disjoint_inode),
        observer.commit(disjoint_policy),
    )
    .await;
    require_committed(
        "concurrent disjoint inode commit",
        inode_outcome
            .map_err(|error| StateStoreConformanceError::adapter("disjoint inode", error))?,
    )?;
    require_committed(
        "concurrent disjoint policy commit",
        policy_outcome
            .map_err(|error| StateStoreConformanceError::adapter("disjoint policy", error))?,
    )?;
    Ok(())
}

async fn join2<A: Future, B: Future>(first: A, second: B) -> (A::Output, B::Output) {
    let mut first = core::pin::pin!(first);
    let mut second = core::pin::pin!(second);
    let mut first_output = None;
    let mut second_output = None;
    core::future::poll_fn(|context| {
        if first_output.is_none()
            && let core::task::Poll::Ready(output) = first.as_mut().poll(context)
        {
            first_output = Some(output);
        }
        if second_output.is_none()
            && let core::task::Poll::Ready(output) = second.as_mut().poll(context)
        {
            second_output = Some(output);
        }
        match (first_output.take(), second_output.take()) {
            (Some(first), Some(second)) => core::task::Poll::Ready((first, second)),
            (first, second) => {
                first_output = first;
                second_output = second;
                core::task::Poll::Pending
            }
        }
    })
    .await
}

fn conformance_request(
    filesystem_id: FilesystemId,
    mutation_number: u128,
    fence: WriterFence,
    preconditions: Vec<Precondition>,
    changes: Vec<StateChange>,
    limits: StateLimits,
) -> Result<CommitRequest, StateStoreConformanceError> {
    CommitRequest::new(
        filesystem_id,
        MutationContext::new(
            w9pt_storage::MutationId::from_u128(mutation_number),
            crate::RequestFingerprint::blake3(&mutation_number.to_be_bytes()),
            ClientIncarnationId::from_u128(304),
            MutationRetention::new(100),
        ),
        fence,
        preconditions,
        changes,
        terminal_result(b"r", limits)?,
        limits,
    )
    .map_err(|error| StateStoreConformanceError::adapter("build conformance commit", error))
}

async fn expect_commit_limit<S: FilesystemStateStore>(
    store: &S,
    request: CommitRequest,
    expected: crate::StateLimitKind,
    message: &'static str,
) -> Result<(), StateStoreConformanceError> {
    if matches!(
        store
            .commit(request)
            .await
            .map_err(|error| StateStoreConformanceError::adapter("commit limit", error))?,
        CommitOutcome::MalformedRequest(crate::MalformedCommit::Limit(
            crate::StateLimitError { kind, .. }
        )) if kind == expected
    ) {
        Ok(())
    } else {
        Err(StateStoreConformanceError::assertion(message))
    }
}

async fn check_receiver_limits<S: FilesystemStateStore>(
    store: &S,
    filesystem_id: FilesystemId,
    root_id: InodeId,
    fence: WriterFence,
    scope: WriterScopeId,
    limits: StateLimits,
) -> Result<(), StateStoreConformanceError> {
    if limits.max_entry_name_bytes() < 4_096 {
        let mut values = limits.values();
        values.max_entry_name_bytes = values
            .max_entry_name_bytes
            .checked_add(1)
            .ok_or_else(|| StateStoreConformanceError::assertion("entry-name limit overflow"))?;
        let looser = StateLimits::new(values).map_err(|error| {
            StateStoreConformanceError::adapter("build looser read limits", error)
        })?;
        let name = EntryName::new(vec![b'n'; values.max_entry_name_bytes], looser)
            .map_err(|error| StateStoreConformanceError::adapter("build oversized name", error))?;
        let request = ReadBatch::new(
            filesystem_id,
            ReadConsistency::LatestLinearizable,
            vec![ReadQuery::DirectoryEntry {
                parent_inode_id: root_id,
                name,
            }],
            looser,
        )
        .map_err(|error| StateStoreConformanceError::adapter("build oversized read", error))?;
        if !matches!(
            store
                .read(request)
                .await
                .map_err(|error| StateStoreConformanceError::adapter("oversized read", error))?,
            ReadOutcome::MalformedRequest(crate::StateLimitError {
                kind: crate::StateLimitKind::EntryName,
                ..
            })
        ) {
            return Err(StateStoreConformanceError::assertion(
                "adapter accepted a point key built under looser name limits",
            ));
        }
    }

    if limits.max_xattr_name_bytes() < 4_096 {
        let mut values = limits.values();
        values.max_xattr_name_bytes = values
            .max_xattr_name_bytes
            .checked_add(1)
            .ok_or_else(|| StateStoreConformanceError::assertion("xattr-name limit overflow"))?;
        let looser = StateLimits::new(values).map_err(|error| {
            StateStoreConformanceError::adapter("build looser commit limits", error)
        })?;
        let name = XattrName::new(vec![b'x'; values.max_xattr_name_bytes], looser)
            .map_err(|error| StateStoreConformanceError::adapter("build oversized xattr", error))?;
        let request = CommitRequest::new(
            filesystem_id,
            MutationContext::new(
                w9pt_storage::MutationId::from_u128(500),
                crate::RequestFingerprint::blake3(b"oversized-xattr"),
                ClientIncarnationId::from_u128(501),
                MutationRetention::new(100),
            ),
            fence,
            vec![],
            vec![StateChange::Insert {
                key: RecordKey::Xattr(filesystem_id, root_id, name.clone()),
                record: StateRecord::Xattr(XattrRecord::new(
                    root_id,
                    name,
                    XattrValue::new(Vec::new(), looser).map_err(|error| {
                        StateStoreConformanceError::adapter("build empty xattr", error)
                    })?,
                    RecordRevision::new(1).expect("one is nonzero"),
                )),
            }],
            terminal_result(b"r", looser)?,
            looser,
        )
        .map_err(|error| StateStoreConformanceError::adapter("build oversized commit", error))?;
        if !matches!(
            store
                .commit(request)
                .await
                .map_err(|error| StateStoreConformanceError::adapter("oversized commit", error))?,
            CommitOutcome::MalformedRequest(crate::MalformedCommit::Limit(
                crate::StateLimitError {
                    kind: crate::StateLimitKind::XattrName,
                    ..
                }
            ))
        ) {
            return Err(StateStoreConformanceError::assertion(
                "adapter accepted a record built under looser xattr limits",
            ));
        }
    }

    if limits.max_principal_bytes() < 4_096 {
        let mut values = limits.values();
        values.max_principal_bytes += 1;
        let looser = StateLimits::new(values).map_err(|error| {
            StateStoreConformanceError::adapter("looser principal limit", error)
        })?;
        let inode = InodeRecord::new(
            InodeId::from_u128(520),
            RecordRevision::new(1).expect("one is nonzero"),
            0o755,
            PrincipalId::new(vec![b'p'; values.max_principal_bytes], looser).map_err(|error| {
                StateStoreConformanceError::adapter("oversized principal", error)
            })?,
            GroupId::new(b"g".to_vec(), looser)
                .map_err(|error| StateStoreConformanceError::adapter("limit group", error))?,
            InodeTimes {
                accessed: UnixTimestamp::new(0, 0).expect("valid timestamp"),
                modified: UnixTimestamp::new(0, 0).expect("valid timestamp"),
                changed: UnixTimestamp::new(0, 0).expect("valid timestamp"),
                created: UnixTimestamp::new(0, 0).expect("valid timestamp"),
            },
            0,
            1,
            InodeGeneration::new(1).expect("one is nonzero"),
            InodeData::Directory {
                generation: DirectoryGeneration::new(1).expect("one is nonzero"),
            },
        )
        .map_err(|error| StateStoreConformanceError::adapter("principal inode", error))?;
        let request = conformance_request(
            filesystem_id,
            521,
            fence,
            vec![],
            vec![StateChange::Insert {
                key: RecordKey::Inode(filesystem_id, inode.inode_id()),
                record: StateRecord::Inode(inode),
            }],
            looser,
        )?;
        expect_commit_limit(
            store,
            request,
            crate::StateLimitKind::Principal,
            "adapter accepted oversized principal",
        )
        .await?;
    }

    if limits.max_group_bytes() < 4_096 {
        let mut values = limits.values();
        values.max_group_bytes += 1;
        let looser = StateLimits::new(values)
            .map_err(|error| StateStoreConformanceError::adapter("looser group limit", error))?;
        let inode_id = InodeId::from_u128(522);
        let inode = InodeRecord::new(
            inode_id,
            RecordRevision::new(1).expect("one is nonzero"),
            0o755,
            PrincipalId::new(b"p".to_vec(), looser)
                .map_err(|error| StateStoreConformanceError::adapter("limit principal", error))?,
            GroupId::new(vec![b'g'; values.max_group_bytes], looser)
                .map_err(|error| StateStoreConformanceError::adapter("oversized group", error))?,
            InodeTimes {
                accessed: UnixTimestamp::new(0, 0).expect("valid timestamp"),
                modified: UnixTimestamp::new(0, 0).expect("valid timestamp"),
                changed: UnixTimestamp::new(0, 0).expect("valid timestamp"),
                created: UnixTimestamp::new(0, 0).expect("valid timestamp"),
            },
            0,
            1,
            InodeGeneration::new(1).expect("one is nonzero"),
            InodeData::Directory {
                generation: DirectoryGeneration::new(1).expect("one is nonzero"),
            },
        )
        .map_err(|error| StateStoreConformanceError::adapter("group inode", error))?;
        let request = conformance_request(
            filesystem_id,
            523,
            fence,
            vec![],
            vec![StateChange::Insert {
                key: RecordKey::Inode(filesystem_id, inode_id),
                record: StateRecord::Inode(inode),
            }],
            looser,
        )?;
        expect_commit_limit(
            store,
            request,
            crate::StateLimitKind::Group,
            "adapter accepted oversized group",
        )
        .await?;
    }

    if limits.max_symlink_bytes() < 32 * 1024 {
        let mut values = limits.values();
        values.max_symlink_bytes += 1;
        let looser = StateLimits::new(values)
            .map_err(|error| StateStoreConformanceError::adapter("looser symlink limit", error))?;
        let inode_id = InodeId::from_u128(524);
        let target = SymlinkTarget::new(vec![b't'; values.max_symlink_bytes], looser)
            .map_err(|error| StateStoreConformanceError::adapter("oversized symlink", error))?;
        let inode = InodeRecord::new(
            inode_id,
            RecordRevision::new(1).expect("one is nonzero"),
            0o777,
            PrincipalId::new(b"p".to_vec(), looser)
                .map_err(|error| StateStoreConformanceError::adapter("symlink principal", error))?,
            GroupId::new(b"g".to_vec(), looser)
                .map_err(|error| StateStoreConformanceError::adapter("symlink group", error))?,
            InodeTimes {
                accessed: UnixTimestamp::new(0, 0).expect("valid timestamp"),
                modified: UnixTimestamp::new(0, 0).expect("valid timestamp"),
                changed: UnixTimestamp::new(0, 0).expect("valid timestamp"),
                created: UnixTimestamp::new(0, 0).expect("valid timestamp"),
            },
            u64::try_from(target.as_bytes().len()).expect("bounded target length fits u64"),
            1,
            InodeGeneration::new(1).expect("one is nonzero"),
            InodeData::Symlink { target },
        )
        .map_err(|error| StateStoreConformanceError::adapter("symlink inode", error))?;
        let request = conformance_request(
            filesystem_id,
            525,
            fence,
            vec![],
            vec![StateChange::Insert {
                key: RecordKey::Inode(filesystem_id, inode_id),
                record: StateRecord::Inode(inode),
            }],
            looser,
        )?;
        expect_commit_limit(
            store,
            request,
            crate::StateLimitKind::Symlink,
            "adapter accepted oversized symlink",
        )
        .await?;
    }

    if limits.max_xattr_value_bytes() < 128 * 1024 {
        let mut values = limits.values();
        values.max_xattr_value_bytes += 1;
        let looser = StateLimits::new(values).map_err(|error| {
            StateStoreConformanceError::adapter("looser xattr-value limit", error)
        })?;
        let name = XattrName::new(b"v".to_vec(), looser)
            .map_err(|error| StateStoreConformanceError::adapter("value xattr name", error))?;
        let record = XattrRecord::new(
            root_id,
            name.clone(),
            XattrValue::new(vec![0; values.max_xattr_value_bytes], looser).map_err(|error| {
                StateStoreConformanceError::adapter("oversized xattr value", error)
            })?,
            RecordRevision::new(1).expect("one is nonzero"),
        );
        let request = conformance_request(
            filesystem_id,
            526,
            fence,
            vec![],
            vec![StateChange::Insert {
                key: RecordKey::Xattr(filesystem_id, root_id, name),
                record: StateRecord::Xattr(record),
            }],
            looser,
        )?;
        expect_commit_limit(
            store,
            request,
            crate::StateLimitKind::XattrValue,
            "adapter accepted oversized xattr value",
        )
        .await?;
    }

    if limits.max_lease_duration_ticks() < u64::MAX {
        let mut values = limits.values();
        values.max_lease_duration_ticks += 1;
        let looser = StateLimits::new(values).map_err(|error| {
            StateStoreConformanceError::adapter("build looser lease limits", error)
        })?;
        let request = AcquireWriterLease::new(
            filesystem_id,
            LeaseOperationId::from_u128(122),
            scope,
            WriterIncarnationId::from_u128(123),
            LeaseId::from_u128(124),
            LeaseDuration::new(values.max_lease_duration_ticks)
                .expect("looser duration is nonzero"),
            looser,
        )
        .map_err(|error| StateStoreConformanceError::adapter("build oversized lease", error))?;
        if !matches!(
            store
                .acquire_writer_lease(request)
                .await
                .map_err(|error| StateStoreConformanceError::adapter("oversized lease", error))?,
            AcquireLeaseOutcome::Rejected(LeaseRejection::InvalidDuration)
        ) {
            return Err(StateStoreConformanceError::assertion(
                "adapter accepted a lease built under looser duration limits",
            ));
        }
    }

    if limits.max_read_queries() < 4_096 {
        let mut values = limits.values();
        values.max_read_queries += 1;
        let looser = StateLimits::new(values)
            .map_err(|error| StateStoreConformanceError::adapter("looser read count", error))?;
        let request = ReadBatch::new(
            filesystem_id,
            ReadConsistency::LatestLinearizable,
            vec![ReadQuery::Filesystem; usize::try_from(values.max_read_queries).unwrap()],
            looser,
        )
        .map_err(|error| StateStoreConformanceError::adapter("oversized read count", error))?;
        if !matches!(
            store
                .read(request)
                .await
                .map_err(|error| StateStoreConformanceError::adapter("read count limit", error))?,
            ReadOutcome::MalformedRequest(crate::StateLimitError {
                kind: crate::StateLimitKind::ReadQueries,
                ..
            })
        ) {
            return Err(StateStoreConformanceError::assertion(
                "adapter accepted too many read queries",
            ));
        }
    }

    if limits.max_scan_items() < u32::MAX {
        let mut values = limits.values();
        values.max_scan_items += 1;
        let looser = StateLimits::new(values)
            .map_err(|error| StateStoreConformanceError::adapter("looser scan count", error))?;
        let bounds = ScanBounds::new(values.max_scan_items, 1, looser)
            .map_err(|error| StateStoreConformanceError::adapter("oversized scan count", error))?;
        let request = ReadBatch::new(
            filesystem_id,
            ReadConsistency::LatestLinearizable,
            vec![ReadQuery::Scan(RecordScan::Inodes {
                after: None,
                bounds,
            })],
            looser,
        )
        .map_err(|error| StateStoreConformanceError::adapter("build oversized scan", error))?;
        if !matches!(
            store
                .read(request)
                .await
                .map_err(|error| StateStoreConformanceError::adapter("scan count limit", error))?,
            ReadOutcome::MalformedRequest(crate::StateLimitError {
                kind: crate::StateLimitKind::ScanItems,
                ..
            })
        ) {
            return Err(StateStoreConformanceError::assertion(
                "adapter accepted an excessive scan item bound",
            ));
        }
    }

    if limits.max_scan_bytes() < usize::MAX {
        let mut values = limits.values();
        values.max_scan_bytes += 1;
        let looser = StateLimits::new(values)
            .map_err(|error| StateStoreConformanceError::adapter("looser scan bytes", error))?;
        let bounds = ScanBounds::new(1, values.max_scan_bytes, looser)
            .map_err(|error| StateStoreConformanceError::adapter("oversized scan bytes", error))?;
        let request = ReadBatch::new(
            filesystem_id,
            ReadConsistency::LatestLinearizable,
            vec![ReadQuery::Scan(RecordScan::Inodes {
                after: None,
                bounds,
            })],
            looser,
        )
        .map_err(|error| StateStoreConformanceError::adapter("build scan byte request", error))?;
        if !matches!(
            store
                .read(request)
                .await
                .map_err(|error| StateStoreConformanceError::adapter("scan byte limit", error))?,
            ReadOutcome::MalformedRequest(crate::StateLimitError {
                kind: crate::StateLimitKind::ScanBytes,
                ..
            })
        ) {
            return Err(StateStoreConformanceError::assertion(
                "adapter accepted an excessive scan byte bound",
            ));
        }
    }

    if limits.max_mutation_result_bytes() < 4 * 1024 * 1024
        && limits.max_mutation_result_bytes() < limits.max_transaction_bytes()
    {
        let mut values = limits.values();
        values.max_mutation_result_bytes += 1;
        let looser = StateLimits::new(values)
            .map_err(|error| StateStoreConformanceError::adapter("looser result limit", error))?;
        let result = MutationResult::new(
            MutationResultKind::new(1).expect("one is nonzero"),
            ResultFormatVersion::new(1).expect("one is nonzero"),
            vec![0; values.max_mutation_result_bytes],
            looser,
        )
        .map_err(|error| StateStoreConformanceError::adapter("oversized result", error))?;
        let request = CommitRequest::new(
            filesystem_id,
            MutationContext::new(
                w9pt_storage::MutationId::from_u128(510),
                crate::RequestFingerprint::blake3(b"oversized-result"),
                ClientIncarnationId::from_u128(511),
                MutationRetention::new(100),
            ),
            fence,
            vec![],
            vec![StateChange::BumpInodeGeneration(root_id)],
            result,
            looser,
        )
        .map_err(|error| StateStoreConformanceError::adapter("build oversized result", error))?;
        if !matches!(
            store
                .commit(request)
                .await
                .map_err(|error| StateStoreConformanceError::adapter("result limit", error))?,
            CommitOutcome::MalformedRequest(crate::MalformedCommit::Limit(
                crate::StateLimitError {
                    kind: crate::StateLimitKind::MutationResult,
                    ..
                }
            ))
        ) {
            return Err(StateStoreConformanceError::assertion(
                "adapter accepted an oversized terminal result",
            ));
        }
    }

    if limits.max_preconditions() < 4_096 {
        let mut values = limits.values();
        values.max_preconditions += 1;
        let looser = StateLimits::new(values).map_err(|error| {
            StateStoreConformanceError::adapter("looser precondition limit", error)
        })?;
        let request = conformance_request(
            filesystem_id,
            512,
            fence,
            vec![
                Precondition::ExactFence(fence);
                usize::try_from(values.max_preconditions).unwrap()
            ],
            vec![StateChange::BumpInodeGeneration(root_id)],
            looser,
        )?;
        if !matches!(
            store
                .commit(request)
                .await
                .map_err(|error| StateStoreConformanceError::adapter(
                    "precondition limit",
                    error
                ))?,
            CommitOutcome::MalformedRequest(crate::MalformedCommit::Limit(
                crate::StateLimitError {
                    kind: crate::StateLimitKind::Preconditions,
                    ..
                }
            ))
        ) {
            return Err(StateStoreConformanceError::assertion(
                "adapter accepted too many preconditions",
            ));
        }
    }

    if limits.max_changes() < 4_096 {
        let mut values = limits.values();
        values.max_changes += 1;
        values.max_change_keys = values
            .max_change_keys
            .max(values.max_changes.saturating_add(1));
        let looser = StateLimits::new(values)
            .map_err(|error| StateStoreConformanceError::adapter("looser change limit", error))?;
        let changes: Vec<_> = (0..values.max_changes)
            .map(|index| {
                StateChange::Delete(RecordKey::Inode(
                    filesystem_id,
                    InodeId::from_u128(10_000 + u128::from(index)),
                ))
            })
            .collect();
        let request = conformance_request(filesystem_id, 513, fence, vec![], changes, looser)?;
        if !matches!(
            store
                .commit(request)
                .await
                .map_err(|error| StateStoreConformanceError::adapter("change limit", error))?,
            CommitOutcome::MalformedRequest(crate::MalformedCommit::Limit(
                crate::StateLimitError {
                    kind: crate::StateLimitKind::Changes,
                    ..
                }
            ))
        ) {
            return Err(StateStoreConformanceError::assertion(
                "adapter accepted too many state changes",
            ));
        }
    }

    if limits.max_change_history_commits() < u32::MAX {
        let mut values = limits.values();
        values.max_change_history_commits += 1;
        let looser = StateLimits::new(values)
            .map_err(|error| StateStoreConformanceError::adapter("looser poll limit", error))?;
        let request = ChangePoll::new(
            filesystem_id,
            ChangeCursor::after(StateRevision::new(1).expect("one is nonzero")),
            values.max_change_history_commits,
            1,
            looser,
        )
        .map_err(|error| StateStoreConformanceError::adapter("oversized poll", error))?;
        if !matches!(
            store
                .poll_changes(request)
                .await
                .map_err(|error| StateStoreConformanceError::adapter("poll limit", error))?,
            ChangePollOutcome::MalformedRequest(crate::InvalidChangeRequest::Limit(
                crate::StateLimitError {
                    kind: crate::StateLimitKind::ChangeHistory,
                    ..
                }
            ))
        ) {
            return Err(StateStoreConformanceError::assertion(
                "adapter accepted an excessive change-poll bound",
            ));
        }
    }
    Ok(())
}

async fn check_record_set_semantics<S: FilesystemStateStore>(
    store: &S,
    filesystem_id: FilesystemId,
    root_id: InodeId,
    fence: WriterFence,
    times: InodeTimes,
    limits: StateLimits,
) -> Result<(), StateStoreConformanceError> {
    let file_id = InodeId::from_u128(20);
    let content_file_id = w9pt_storage::FileId::from_u128(20);
    let mut entry_name = EntryName::new(b"f".to_vec(), limits)
        .map_err(|error| StateStoreConformanceError::adapter("build entry name", error))?;
    let file = conformance_file(file_id, content_file_id, 1, 1, None, times, limits)?;
    let root = conformance_root(root_id, 3, 2, times, limits)?;
    let entry = DirectoryEntryRecord::new(
        root_id,
        entry_name.clone(),
        DirectoryCookie::new(1),
        file_id,
        RecordRevision::new(1).expect("one is a valid revision"),
    )
    .map_err(|error| StateStoreConformanceError::adapter("build directory entry", error))?;
    let create = commit_changes(
        store,
        filesystem_id,
        100,
        fence,
        vec![
            StateChange::Insert {
                key: RecordKey::Inode(filesystem_id, file_id),
                record: StateRecord::Inode(file),
            },
            StateChange::Insert {
                key: RecordKey::DirectoryEntry(filesystem_id, root_id, entry_name.clone()),
                record: StateRecord::DirectoryEntry(entry),
            },
            StateChange::Replace {
                key: RecordKey::Inode(filesystem_id, root_id),
                record: StateRecord::Inode(root),
            },
            StateChange::AdvanceDirectoryCookie {
                count: core::num::NonZeroU64::new(1).expect("one is nonzero"),
            },
        ],
        limits,
    )
    .await?;
    require_committed("create record set", create)?;

    let renamed = EntryName::new(b"n".to_vec(), limits)
        .map_err(|error| StateStoreConformanceError::adapter("build rename target", error))?;
    let renamed_entry = DirectoryEntryRecord::new(
        root_id,
        renamed.clone(),
        DirectoryCookie::new(1),
        file_id,
        RecordRevision::new(1).expect("one is nonzero"),
    )
    .map_err(|error| StateStoreConformanceError::adapter("build renamed entry", error))?;
    require_committed(
        "rename record set",
        commit_changes(
            store,
            filesystem_id,
            120,
            fence,
            vec![
                StateChange::Delete(RecordKey::DirectoryEntry(
                    filesystem_id,
                    root_id,
                    entry_name,
                )),
                StateChange::Insert {
                    key: RecordKey::DirectoryEntry(filesystem_id, root_id, renamed.clone()),
                    record: StateRecord::DirectoryEntry(renamed_entry),
                },
                StateChange::Replace {
                    key: RecordKey::Inode(filesystem_id, root_id),
                    record: StateRecord::Inode(conformance_root(root_id, 4, 3, times, limits)?),
                },
            ],
            limits,
        )
        .await?,
    )?;
    entry_name = renamed;

    let alias = EntryName::new(b"l".to_vec(), limits)
        .map_err(|error| StateStoreConformanceError::adapter("build link name", error))?;
    let alias_entry = DirectoryEntryRecord::new(
        root_id,
        alias.clone(),
        DirectoryCookie::new(2),
        file_id,
        RecordRevision::new(1).expect("one is nonzero"),
    )
    .map_err(|error| StateStoreConformanceError::adapter("build hard link", error))?;
    require_committed(
        "hard-link record set",
        commit_changes(
            store,
            filesystem_id,
            121,
            fence,
            vec![
                StateChange::Insert {
                    key: RecordKey::DirectoryEntry(filesystem_id, root_id, alias.clone()),
                    record: StateRecord::DirectoryEntry(alias_entry),
                },
                StateChange::Replace {
                    key: RecordKey::Inode(filesystem_id, file_id),
                    record: StateRecord::Inode(conformance_file(
                        file_id,
                        content_file_id,
                        2,
                        2,
                        None,
                        times,
                        limits,
                    )?),
                },
                StateChange::Replace {
                    key: RecordKey::Inode(filesystem_id, root_id),
                    record: StateRecord::Inode(conformance_root(root_id, 5, 4, times, limits)?),
                },
                StateChange::AdvanceDirectoryCookie {
                    count: core::num::NonZeroU64::new(1).expect("one is nonzero"),
                },
            ],
            limits,
        )
        .await?,
    )?;
    require_committed(
        "unlink hard-link record set",
        commit_changes(
            store,
            filesystem_id,
            122,
            fence,
            vec![
                StateChange::Delete(RecordKey::DirectoryEntry(filesystem_id, root_id, alias)),
                StateChange::Replace {
                    key: RecordKey::Inode(filesystem_id, file_id),
                    record: StateRecord::Inode(conformance_file(
                        file_id,
                        content_file_id,
                        1,
                        3,
                        None,
                        times,
                        limits,
                    )?),
                },
                StateChange::Replace {
                    key: RecordKey::Inode(filesystem_id, root_id),
                    record: StateRecord::Inode(conformance_root(root_id, 6, 5, times, limits)?),
                },
            ],
            limits,
        )
        .await?,
    )?;

    let content_mutation = w9pt_storage::MutationId::from_u128(101);
    let repository = w9pt_storage::ContentRepository::new(
        w9pt_storage::testing::MemoryTarget::new(),
        "state-conformance",
        w9pt_storage::CreationDefaults::new(w9pt_storage::StorageMethod::BlockSplit),
        w9pt_storage::StorageLimits::default(),
    )
    .map_err(|error| StateStoreConformanceError::adapter("build content repository", error))?;
    let prepared = repository
        .prepare_create(content_file_id, content_mutation, 0, b"d")
        .await
        .map_err(|error| StateStoreConformanceError::adapter("prepare content", error))?;
    let publish_request = CommitRequest::new(
        filesystem_id,
        MutationContext::new(
            content_mutation,
            crate::RequestFingerprint::blake3(b"publish"),
            ClientIncarnationId::from_u128(102),
            MutationRetention::new(100),
        ),
        fence,
        vec![],
        vec![StateChange::PublishContent(PublishContent {
            inode_id: file_id,
            expected_base: w9pt_storage::BaseContentIdentity::NEW_FILE,
            logical_size: prepared.content().logical_size(),
            data_generation: DataGeneration::new(prepared.content().generation())
                .expect("prepared generation is nonzero"),
            prepared: prepared.clone(),
            inode_generation: InodeGeneration::new(4).expect("four is nonzero"),
            times,
        })],
        terminal_result(b"r", limits)?,
        limits,
    )
    .map_err(|error| StateStoreConformanceError::adapter("build content publish", error))?;
    let published = store
        .commit(publish_request)
        .await
        .map_err(|error| StateStoreConformanceError::adapter("publish content", error))?;
    require_committed("publish content", published)?;

    let changed_timestamp = UnixTimestamp::new(1, 0).expect("valid timestamp");
    let changed_times = InodeTimes {
        accessed: changed_timestamp,
        modified: changed_timestamp,
        changed: changed_timestamp,
        created: times.created,
    };
    let setattr = InodeRecord::new(
        file_id,
        RecordRevision::new(1).expect("one is nonzero"),
        0o600,
        PrincipalId::new(b"o".to_vec(), limits)
            .map_err(|error| StateStoreConformanceError::adapter("build setattr owner", error))?,
        GroupId::new(b"q".to_vec(), limits)
            .map_err(|error| StateStoreConformanceError::adapter("build setattr group", error))?,
        changed_times,
        prepared.content().logical_size(),
        1,
        InodeGeneration::new(5).expect("five is nonzero"),
        InodeData::RegularFile {
            content_file_id,
            content: Some(prepared.content().clone()),
            data_generation: prepared.content().generation(),
        },
    )
    .map_err(|error| StateStoreConformanceError::adapter("build setattr inode", error))?;
    require_committed(
        "simultaneous setattr",
        commit_changes(
            store,
            filesystem_id,
            123,
            fence,
            vec![StateChange::Replace {
                key: RecordKey::Inode(filesystem_id, file_id),
                record: StateRecord::Inode(setattr),
            }],
            limits,
        )
        .await?,
    )?;

    let first_open = OpenId::from_u128(103);
    let second_open = OpenId::from_u128(104);
    let first_client = ClientIncarnationId::from_u128(105);
    let second_client = ClientIncarnationId::from_u128(106);
    let revision = RecordRevision::new(1).expect("one is a valid revision");
    let opens = commit_changes(
        store,
        filesystem_id,
        107,
        fence,
        vec![
            StateChange::Insert {
                key: RecordKey::Open(filesystem_id, first_open),
                record: StateRecord::Open(OpenRecord::new(
                    first_open,
                    file_id,
                    first_client,
                    OpenAccess::ReadWrite,
                    false,
                    InodeGeneration::new(5).expect("five is nonzero"),
                    revision,
                )),
            },
            StateChange::Insert {
                key: RecordKey::OpenPin(filesystem_id, file_id, first_open),
                record: StateRecord::OpenPin(OpenPinRecord::new(file_id, first_open, revision)),
            },
            StateChange::Insert {
                key: RecordKey::Open(filesystem_id, second_open),
                record: StateRecord::Open(OpenRecord::new(
                    second_open,
                    file_id,
                    second_client,
                    OpenAccess::ReadWrite,
                    false,
                    InodeGeneration::new(5).expect("five is nonzero"),
                    revision,
                )),
            },
            StateChange::Insert {
                key: RecordKey::OpenPin(filesystem_id, file_id, second_open),
                record: StateRecord::OpenPin(OpenPinRecord::new(file_id, second_open, revision)),
            },
        ],
        limits,
    )
    .await?;
    require_committed("open pins", opens)?;

    let lock_id = LockId::from_u128(108);
    let first_lock = LockRecord::new(
        lock_id,
        file_id,
        LockRange::finite(0, 2).expect("finite lock range"),
        LockKind::Exclusive,
        LockOwner::new(first_client, first_open),
        LockGeneration::new(1).expect("one is nonzero"),
        revision,
    );
    let locked = commit_changes(
        store,
        filesystem_id,
        109,
        fence,
        vec![StateChange::Insert {
            key: RecordKey::Lock(filesystem_id, file_id, lock_id),
            record: StateRecord::Lock(first_lock),
        }],
        limits,
    )
    .await?;
    require_committed("first lock", locked)?;
    let disjoint_lock_id = LockId::from_u128(124);
    let disjoint_lock = LockRecord::new(
        disjoint_lock_id,
        file_id,
        LockRange::finite(2, 3).expect("finite lock range"),
        LockKind::Shared,
        LockOwner::new(second_client, second_open),
        LockGeneration::new(1).expect("one is nonzero"),
        revision,
    );
    require_committed(
        "disjoint lock",
        commit_changes(
            store,
            filesystem_id,
            125,
            fence,
            vec![StateChange::Insert {
                key: RecordKey::Lock(filesystem_id, file_id, disjoint_lock_id),
                record: StateRecord::Lock(disjoint_lock),
            }],
            limits,
        )
        .await?,
    )?;
    let lock_bounds = ScanBounds::new(1, limits.max_scan_bytes(), limits)
        .map_err(|error| StateStoreConformanceError::adapter("build lock scan", error))?;
    let first_page_request = ReadBatch::new(
        filesystem_id,
        ReadConsistency::LatestLinearizable,
        vec![ReadQuery::Scan(RecordScan::Locks {
            after: None,
            bounds: lock_bounds,
        })],
        limits,
    )
    .map_err(|error| StateStoreConformanceError::adapter("build first lock page", error))?;
    let first_cursor = match store
        .read(first_page_request)
        .await
        .map_err(|error| StateStoreConformanceError::adapter("read first lock page", error))?
    {
        ReadOutcome::Snapshot(snapshot) => match &snapshot.results()[0] {
            ReadResult::Scan(page) if page.records().len() == 1 => match page.resume() {
                Some(ScanResume::Lock(cursor)) => *cursor,
                _ => {
                    return Err(StateStoreConformanceError::assertion(
                        "bounded lock page omitted its stable resume cursor",
                    ));
                }
            },
            _ => {
                return Err(StateStoreConformanceError::assertion(
                    "bounded lock scan returned an invalid first page",
                ));
            }
        },
        outcome => {
            return Err(StateStoreConformanceError::unexpected(
                "read first lock page",
                format!("{outcome:?}"),
            ));
        }
    };
    let second_page_request = ReadBatch::new(
        filesystem_id,
        ReadConsistency::LatestLinearizable,
        vec![ReadQuery::Scan(RecordScan::Locks {
            after: Some(first_cursor),
            bounds: lock_bounds,
        })],
        limits,
    )
    .map_err(|error| StateStoreConformanceError::adapter("build resumed lock page", error))?;
    if !matches!(
        store
            .read(second_page_request)
            .await
            .map_err(|error| StateStoreConformanceError::adapter("resume lock scan", error))?,
        ReadOutcome::Snapshot(snapshot)
            if matches!(&snapshot.results()[0], ReadResult::Scan(page) if page.records().len() == 1)
    ) {
        return Err(StateStoreConformanceError::assertion(
            "stable lock resume cursor did not return the next record",
        ));
    }
    let conflicting = LockRecord::new(
        LockId::from_u128(110),
        file_id,
        LockRange::finite(1, 3).expect("finite lock range"),
        LockKind::Shared,
        LockOwner::new(second_client, second_open),
        LockGeneration::new(1).expect("one is nonzero"),
        revision,
    );
    if !matches!(
        commit_changes(
            store,
            filesystem_id,
            111,
            fence,
            vec![StateChange::Insert {
                key: RecordKey::Lock(filesystem_id, file_id, conflicting.lock_id()),
                record: StateRecord::Lock(conflicting),
            }],
            limits,
        )
        .await?,
        CommitOutcome::Conflict(_)
    ) {
        return Err(StateStoreConformanceError::assertion(
            "overlapping incompatible lock was not rejected",
        ));
    }
    require_committed(
        "remove disjoint lock",
        commit_changes(
            store,
            filesystem_id,
            126,
            fence,
            vec![StateChange::Delete(RecordKey::Lock(
                filesystem_id,
                file_id,
                disjoint_lock_id,
            ))],
            limits,
        )
        .await?,
    )?;

    let staging_id = XattrStagingId::from_u128(112);
    let xattr_name = XattrName::new(b"x".to_vec(), limits)
        .map_err(|error| StateStoreConformanceError::adapter("build staged xattr name", error))?;
    let staging = XattrStagingRecord::new(
        staging_id,
        file_id,
        xattr_name.clone(),
        1,
        XattrValue::new(b"v".to_vec(), limits)
            .map_err(|error| StateStoreConformanceError::adapter("build staged xattr", error))?,
        revision,
        limits,
    )
    .map_err(|error| StateStoreConformanceError::adapter("build xattr staging", error))?;
    require_committed(
        "stage xattr",
        commit_changes(
            store,
            filesystem_id,
            113,
            fence,
            vec![StateChange::Insert {
                key: RecordKey::XattrStaging(filesystem_id, staging_id),
                record: StateRecord::XattrStaging(staging),
            }],
            limits,
        )
        .await?,
    )?;
    require_committed(
        "publish xattr staging",
        commit_changes(
            store,
            filesystem_id,
            114,
            fence,
            vec![StateChange::PublishXattrStaging(PublishXattrStaging {
                staging_id,
                inode_id: file_id,
                name: xattr_name.clone(),
            })],
            limits,
        )
        .await?,
    )?;

    let unlinked_file = conformance_file(
        file_id,
        content_file_id,
        0,
        6,
        Some(prepared.content().clone()),
        times,
        limits,
    )?;
    let unlinked_root = conformance_root(root_id, 7, 6, times, limits)?;
    require_committed(
        "open-unlinked transition",
        commit_changes(
            store,
            filesystem_id,
            115,
            fence,
            vec![
                StateChange::Delete(RecordKey::DirectoryEntry(
                    filesystem_id,
                    root_id,
                    entry_name,
                )),
                StateChange::Replace {
                    key: RecordKey::Inode(filesystem_id, file_id),
                    record: StateRecord::Inode(unlinked_file),
                },
                StateChange::Insert {
                    key: RecordKey::Orphan(filesystem_id, file_id),
                    record: StateRecord::Orphan(
                        OrphanRecord::new(
                            file_id,
                            2,
                            StateRevision::new(1).expect("one is nonzero"),
                            revision,
                        )
                        .map_err(|error| {
                            StateStoreConformanceError::adapter("build orphan", error)
                        })?,
                    ),
                },
                StateChange::Replace {
                    key: RecordKey::Inode(filesystem_id, root_id),
                    record: StateRecord::Inode(unlinked_root),
                },
            ],
            limits,
        )
        .await?,
    )?;
    require_committed(
        "remove xattr",
        commit_changes(
            store,
            filesystem_id,
            116,
            fence,
            vec![StateChange::Delete(RecordKey::Xattr(
                filesystem_id,
                file_id,
                xattr_name,
            ))],
            limits,
        )
        .await?,
    )?;
    require_committed(
        "release first open",
        commit_changes(
            store,
            filesystem_id,
            117,
            fence,
            vec![
                StateChange::Delete(RecordKey::Lock(filesystem_id, file_id, lock_id)),
                StateChange::Delete(RecordKey::Open(filesystem_id, first_open)),
                StateChange::Delete(RecordKey::OpenPin(filesystem_id, file_id, first_open)),
                StateChange::Replace {
                    key: RecordKey::Orphan(filesystem_id, file_id),
                    record: StateRecord::Orphan(
                        OrphanRecord::new(
                            file_id,
                            1,
                            StateRevision::new(1).expect("one is nonzero"),
                            revision,
                        )
                        .map_err(|error| {
                            StateStoreConformanceError::adapter("build retained orphan", error)
                        })?,
                    ),
                },
            ],
            limits,
        )
        .await?,
    )?;
    require_committed(
        "retire final open",
        commit_changes(
            store,
            filesystem_id,
            118,
            fence,
            vec![
                StateChange::Delete(RecordKey::Open(filesystem_id, second_open)),
                StateChange::Delete(RecordKey::OpenPin(filesystem_id, file_id, second_open)),
                StateChange::Delete(RecordKey::Orphan(filesystem_id, file_id)),
                StateChange::Delete(RecordKey::Inode(filesystem_id, file_id)),
            ],
            limits,
        )
        .await?,
    )?;
    Ok(())
}

async fn commit_changes<S: FilesystemStateStore>(
    store: &S,
    filesystem_id: FilesystemId,
    mutation_number: u128,
    fence: WriterFence,
    changes: Vec<StateChange>,
    limits: StateLimits,
) -> Result<CommitOutcome, StateStoreConformanceError> {
    let request = CommitRequest::new(
        filesystem_id,
        MutationContext::new(
            w9pt_storage::MutationId::from_u128(mutation_number),
            crate::RequestFingerprint::blake3(&mutation_number.to_be_bytes()),
            ClientIncarnationId::from_u128(119),
            MutationRetention::new(100),
        ),
        fence,
        vec![],
        changes,
        terminal_result(b"r", limits)?,
        limits,
    )
    .map_err(|error| StateStoreConformanceError::adapter("build semantic commit", error))?;
    store
        .commit(request)
        .await
        .map_err(|error| StateStoreConformanceError::adapter("semantic commit", error))
}

fn require_committed(
    step: &'static str,
    outcome: CommitOutcome,
) -> Result<(), StateStoreConformanceError> {
    if matches!(outcome, CommitOutcome::Committed(_)) {
        Ok(())
    } else {
        Err(StateStoreConformanceError::unexpected(
            step,
            format!("{outcome:?}"),
        ))
    }
}

fn conformance_root(
    root_id: InodeId,
    inode_generation: u64,
    directory_generation: u64,
    times: InodeTimes,
    limits: StateLimits,
) -> Result<InodeRecord, StateStoreConformanceError> {
    InodeRecord::new(
        root_id,
        RecordRevision::new(1).expect("one is nonzero"),
        0o755,
        PrincipalId::new(b"r".to_vec(), limits)
            .map_err(|error| StateStoreConformanceError::adapter("build root owner", error))?,
        GroupId::new(b"g".to_vec(), limits)
            .map_err(|error| StateStoreConformanceError::adapter("build root group", error))?,
        times,
        0,
        1,
        InodeGeneration::new(inode_generation).expect("test generation is nonzero"),
        InodeData::Directory {
            generation: DirectoryGeneration::new(directory_generation)
                .expect("test generation is nonzero"),
        },
    )
    .map_err(|error| StateStoreConformanceError::adapter("build root replacement", error))
}

#[allow(clippy::too_many_arguments)]
fn conformance_file(
    file_id: InodeId,
    content_file_id: w9pt_storage::FileId,
    links: u64,
    inode_generation: u64,
    content: Option<w9pt_storage::ContentRef>,
    times: InodeTimes,
    limits: StateLimits,
) -> Result<InodeRecord, StateStoreConformanceError> {
    let logical_size = content
        .as_ref()
        .map_or(0, w9pt_storage::ContentRef::logical_size);
    let data_generation = content
        .as_ref()
        .map_or(0, w9pt_storage::ContentRef::generation);
    InodeRecord::new(
        file_id,
        RecordRevision::new(1).expect("one is nonzero"),
        0o644,
        PrincipalId::new(b"p".to_vec(), limits)
            .map_err(|error| StateStoreConformanceError::adapter("build file owner", error))?,
        GroupId::new(b"g".to_vec(), limits)
            .map_err(|error| StateStoreConformanceError::adapter("build file group", error))?,
        times,
        logical_size,
        links,
        InodeGeneration::new(inode_generation).expect("test generation is nonzero"),
        InodeData::RegularFile {
            content_file_id,
            content,
            data_generation,
        },
    )
    .map_err(|error| StateStoreConformanceError::adapter("build file inode", error))
}

fn terminal_result(
    bytes: &[u8],
    limits: StateLimits,
) -> Result<MutationResult, StateStoreConformanceError> {
    MutationResult::new(
        MutationResultKind::new(1).expect("one is a valid result kind"),
        ResultFormatVersion::new(1).expect("one is a valid format version"),
        bytes.to_vec(),
        limits,
    )
    .map_err(|error| StateStoreConformanceError::adapter("build terminal result", error))
}

/// Failure from a reusable state-store semantic conformance check.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateStoreConformanceError {
    step: &'static str,
    detail: Box<str>,
}

impl StateStoreConformanceError {
    fn assertion(detail: &'static str) -> Self {
        Self {
            step: "assertion",
            detail: detail.into(),
        }
    }

    fn adapter(step: &'static str, error: impl fmt::Display) -> Self {
        Self {
            step,
            detail: error.to_string().into_boxed_str(),
        }
    }

    fn unexpected(step: &'static str, detail: String) -> Self {
        Self {
            step,
            detail: detail.into_boxed_str(),
        }
    }
}

impl fmt::Display for StateStoreConformanceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "state-store conformance {}: {}",
            self.step, self.detail
        )
    }
}

impl std::error::Error for StateStoreConformanceError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LeaseDeadline, ManualLeaseClock};

    #[test]
    fn memory_authority_passes_reusable_conformance() {
        let authority = super::super::MemoryAuthority::new(
            WriterTopology::SerializableMultiWriter,
            StateLimits::default(),
            ManualLeaseClock::new(LeaseDeadline::new(0)),
        );
        w9pt_storage::testing::block_on(check_state_store_conformance(&authority)).unwrap();
    }

    #[test]
    fn single_writer_memory_authority_passes_reusable_conformance() {
        let authority = super::super::MemoryAuthority::new(
            WriterTopology::SingleFencedWriter,
            StateLimits::default(),
            ManualLeaseClock::new(LeaseDeadline::new(0)),
        );
        w9pt_storage::testing::block_on(check_state_store_conformance(&authority)).unwrap();
    }
}
