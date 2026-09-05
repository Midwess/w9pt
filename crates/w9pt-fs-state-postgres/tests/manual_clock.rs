//! Feature-gated deterministic database clock integration.

#![cfg(feature = "test-support")]

mod common;

use sea_orm::ConnectionTrait;
use w9pt_fs_state::{
    AcquireLeaseOutcome, AcquireWriterLease, FilesystemId, FilesystemStateStore, LeaseDeadline,
    LeaseDuration, LeaseId, LeaseOperationId, StateLimits, WriterIncarnationId, WriterScopeId,
};
use w9pt_fs_state_postgres::{
    PostgresStateConfig, PostgresStateStore,
    testing::{DatabaseManualClock, DatabaseManualClockId, TestLeaseClockSource},
};

use common::{cleanup_filesystems, connect, live_dsn, statement};

#[tokio::test]
async fn independent_pools_drive_lease_expiry_from_one_manual_database_row()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(dsn) = live_dsn() else {
        return Ok(());
    };
    let control_pool = connect(dsn.clone(), 2).await?;
    let state_pool = connect(dsn, 4).await?;
    PostgresStateStore::migrate(&control_pool).await?;
    let filesystem_id = FilesystemId::from_u128(u128::MAX - 405);
    cleanup_filesystems(&control_pool, &[filesystem_id]).await?;
    DatabaseManualClock::install(&control_pool).await?;
    let clock_id = DatabaseManualClockId::from_u128(u128::MAX - 404);
    control_pool
        .execute(statement(
            r#"DELETE FROM "w9pt_fs_state_test_v1"."commit_failures"
               WHERE "control_id" = $1"#,
            vec![clock_id.as_bytes().to_vec().into()],
        ))
        .await?;
    control_pool
        .execute(statement(
            r#"DELETE FROM "w9pt_fs_state_test_v1"."lease_clocks"
               WHERE "clock_id" = $1"#,
            vec![clock_id.as_bytes().to_vec().into()],
        ))
        .await?;
    let clock =
        DatabaseManualClock::initialize(&control_pool, clock_id, LeaseDeadline::new(1_000)).await?;
    let store = PostgresStateStore::open_for_testing(
        state_pool,
        PostgresStateConfig::default(),
        TestLeaseClockSource::Manual(clock),
    )
    .await?;
    let limits = StateLimits::default();
    let scope = WriterScopeId::from_u128(1);
    let holder = WriterIncarnationId::from_u128(2);
    let first = AcquireWriterLease::new(
        filesystem_id,
        LeaseOperationId::from_u128(3),
        scope,
        holder,
        LeaseId::from_u128(4),
        LeaseDuration::new(100)?,
        limits,
    )?;
    let AcquireLeaseOutcome::Granted(first) = store.acquire_writer_lease(first).await? else {
        panic!("first manual-clock lease was not granted");
    };
    assert_eq!(first.deadline, LeaseDeadline::new(1_100));

    clock.advance_time(&control_pool, 100).await?;
    let takeover = AcquireWriterLease::new(
        filesystem_id,
        LeaseOperationId::from_u128(5),
        scope,
        WriterIncarnationId::from_u128(6),
        LeaseId::from_u128(7),
        LeaseDuration::new(100)?,
        limits,
    )?;
    let AcquireLeaseOutcome::Granted(takeover) = store.acquire_writer_lease(takeover).await? else {
        panic!("expired manual-clock lease was not taken over");
    };
    assert_eq!(takeover.deadline, LeaseDeadline::new(1_200));
    assert!(takeover.fence.fencing_token > first.fence.fencing_token);
    Ok(())
}
