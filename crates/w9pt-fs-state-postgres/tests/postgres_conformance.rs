//! Reusable state-store conformance over independent PostgreSQL pools.

#![cfg(feature = "test-support")]

mod common;

use core::fmt;
use std::{
    collections::BTreeSet,
    sync::atomic::{AtomicUsize, Ordering},
};

use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, TransactionTrait};
use w9pt_fs_state::{
    FilesystemId, StateLimits,
    testing::{CommitFailureTiming, StateStoreConformanceHarness, check_state_store_conformance},
};
use w9pt_fs_state_postgres::{
    PostgresConfigError, PostgresOpenError, PostgresStateConfig, PostgresStateStore,
    testing::{
        DatabaseManualClock, DatabaseManualClockError, DatabaseManualClockId, DatabaseTestControl,
        TestLeaseClockSource,
    },
};

use common::{connect, statement};

struct PostgresHarness {
    clients: Vec<PostgresStateStore>,
    next_client: AtomicUsize,
    pool: DatabaseConnection,
    dsn: String,
    clock: DatabaseManualClock,
    control: DatabaseTestControl,
}

impl StateStoreConformanceHarness for PostgresHarness {
    type Store = PostgresStateStore;
    type Error = HarnessError;

    fn open_client(&self) -> Self::Store {
        let index = self.next_client.fetch_add(1, Ordering::Relaxed) % self.clients.len();
        self.clients[index].clone()
    }

    async fn advance_time(&self, ticks: u64) -> Result<(), Self::Error> {
        self.clock.advance_time(&self.pool, ticks).await?;
        Ok(())
    }

    async fn inject_commit_failure(&self, timing: CommitFailureTiming) -> Result<(), Self::Error> {
        self.control
            .inject_commit_failure(&self.pool, timing)
            .await?;
        Ok(())
    }

    async fn compact_change_history(&self) -> Result<(), Self::Error> {
        self.control
            .compact_change_history(&self.pool, &conformance_filesystems())
            .await?;
        Ok(())
    }

    async fn open_isolated_with_limits(
        &self,
        limits: StateLimits,
    ) -> Result<Self::Store, Self::Error> {
        let default = PostgresStateConfig::default();
        let config = PostgresStateConfig::new(
            limits,
            default.statement_timeout(),
            default.lock_timeout(),
            default.definitive_abort_retries(),
            default.ambiguous_commit_recovery_attempts(),
            default.durability(),
        )?;
        let pool = connect(self.dsn.clone(), 4).await?;
        Ok(PostgresStateStore::open_for_testing(
            pool,
            config,
            TestLeaseClockSource::Manual(self.clock),
        )
        .await?)
    }
}

#[derive(Debug)]
enum HarnessError {
    Control(DatabaseManualClockError),
    Open(PostgresOpenError),
    Config(PostgresConfigError),
    Database(DbErr),
    WrongServerVersion { expected: u32, actual: i32 },
}

impl fmt::Display for HarnessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Control(error) => error.fmt(formatter),
            Self::Open(error) => error.fmt(formatter),
            Self::Config(error) => error.fmt(formatter),
            Self::Database(error) => error.fmt(formatter),
            Self::WrongServerVersion { expected, actual } => write!(
                formatter,
                "PostgreSQL conformance DSN expected major {expected}, found {actual}"
            ),
        }
    }
}

impl std::error::Error for HarnessError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Control(error) => Some(error),
            Self::Open(error) => Some(error),
            Self::Config(error) => Some(error),
            Self::Database(error) => Some(error),
            Self::WrongServerVersion { .. } => None,
        }
    }
}

impl From<DatabaseManualClockError> for HarnessError {
    fn from(error: DatabaseManualClockError) -> Self {
        Self::Control(error)
    }
}

impl From<PostgresOpenError> for HarnessError {
    fn from(error: PostgresOpenError) -> Self {
        Self::Open(error)
    }
}

impl From<PostgresConfigError> for HarnessError {
    fn from(error: PostgresConfigError) -> Self {
        Self::Config(error)
    }
}

impl From<DbErr> for HarnessError {
    fn from(error: DbErr) -> Self {
        Self::Database(error)
    }
}

async fn open_harness(
    dsn: &str,
    expected_major: Option<u32>,
    clock_ordinal: u128,
) -> Result<PostgresHarness, HarnessError> {
    let pool = connect(dsn.to_owned(), 16).await?;
    PostgresStateStore::migrate(&pool).await?;
    let row = pool
        .query_one(statement(
            r#"SELECT current_setting('server_version_num')::integer / 10000 AS major"#,
            Vec::new(),
        ))
        .await?
        .ok_or_else(|| DbErr::RecordNotFound("server version query returned no row".to_owned()))?;
    let actual_major: i32 = row.try_get("", "major")?;
    if let Some(expected) = expected_major
        && actual_major != i32::try_from(expected).unwrap_or(i32::MAX)
    {
        return Err(HarnessError::WrongServerVersion {
            expected,
            actual: actual_major,
        });
    }
    DatabaseManualClock::install(&pool).await?;
    reset_conformance_state(&pool).await?;
    let clock_id = DatabaseManualClockId::from_u128(u128::MAX - clock_ordinal);
    pool.execute(statement(
        r#"DELETE FROM "w9pt_fs_state_test_v1"."commit_failures"
           WHERE "control_id" = $1"#,
        vec![clock_id.as_bytes().to_vec().into()],
    ))
    .await?;
    pool.execute(statement(
        r#"DELETE FROM "w9pt_fs_state_test_v1"."lease_clocks"
           WHERE "clock_id" = $1"#,
        vec![clock_id.as_bytes().to_vec().into()],
    ))
    .await?;
    let clock =
        DatabaseManualClock::initialize(&pool, clock_id, w9pt_fs_state::LeaseDeadline::new(1_000))
            .await?;
    let mut clients = Vec::with_capacity(4);
    for _ in 0..4 {
        let client_pool = connect(dsn.to_owned(), 4).await?;
        clients.push(
            PostgresStateStore::open_for_testing(
                client_pool,
                PostgresStateConfig::default(),
                TestLeaseClockSource::Manual(clock),
            )
            .await?,
        );
    }
    Ok(PostgresHarness {
        clients,
        next_client: AtomicUsize::new(0),
        pool,
        dsn: dsn.to_owned(),
        clock,
        control: DatabaseTestControl::for_clock(clock),
    })
}

fn conformance_filesystems() -> Vec<FilesystemId> {
    [1u128, 600, 700, 720]
        .into_iter()
        .map(FilesystemId::from_u128)
        .collect()
}

fn should_run_target(expected_major: Option<u32>, dsn: &str, seen: &mut BTreeSet<String>) -> bool {
    let first_use = seen.insert(dsn.to_owned());
    expected_major.is_some() || first_use
}

async fn reset_conformance_state(pool: &DatabaseConnection) -> Result<(), DbErr> {
    const DELETIONS: &[&str] = &[
        r#"DELETE FROM "public"."w9pt_fs_state_change_keys" WHERE "filesystem_id" = $1"#,
        r#"DELETE FROM "public"."w9pt_fs_state_change_commits" WHERE "filesystem_id" = $1"#,
        r#"DELETE FROM "public"."w9pt_fs_state_locks" WHERE "filesystem_id" = $1"#,
        r#"DELETE FROM "public"."w9pt_fs_state_open_pins" WHERE "filesystem_id" = $1"#,
        r#"DELETE FROM "public"."w9pt_fs_state_orphans" WHERE "filesystem_id" = $1"#,
        r#"DELETE FROM "public"."w9pt_fs_state_xattrs" WHERE "filesystem_id" = $1"#,
        r#"DELETE FROM "public"."w9pt_fs_state_xattr_staging" WHERE "filesystem_id" = $1"#,
        r#"DELETE FROM "public"."w9pt_fs_state_directory_entries" WHERE "filesystem_id" = $1"#,
        r#"DELETE FROM "public"."w9pt_fs_state_opens" WHERE "filesystem_id" = $1"#,
        r#"DELETE FROM "public"."w9pt_fs_state_filesystem_records" WHERE "filesystem_id" = $1"#,
        r#"DELETE FROM "public"."w9pt_fs_state_inodes" WHERE "filesystem_id" = $1"#,
        r#"DELETE FROM "public"."w9pt_fs_state_mutation_results" WHERE "filesystem_id" = $1"#,
        r#"DELETE FROM "public"."w9pt_fs_state_writer_lease_operations" WHERE "filesystem_id" = $1"#,
        r#"DELETE FROM "public"."w9pt_fs_state_writer_fences" WHERE "filesystem_id" = $1"#,
        r#"DELETE FROM "public"."w9pt_fs_state_authority_heads" WHERE "filesystem_id" = $1"#,
    ];
    let transaction = pool.begin().await?;
    for filesystem_id in conformance_filesystems() {
        let filesystem = filesystem_id.as_bytes().to_vec();
        for statement in DELETIONS {
            transaction
                .execute(common::statement(
                    *statement,
                    vec![filesystem.clone().into()],
                ))
                .await?;
        }
    }
    transaction.commit().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn postgres_state_store_conformance_matrix() -> Result<(), Box<dyn std::error::Error>> {
    let required = std::env::var("W9PT_POSTGRES_TEST_REQUIRED").as_deref() == Ok("1");
    let mut targets = Vec::new();
    for major in [15u32, 16, 17, 18] {
        let name = format!("W9PT_POSTGRES_{major}_DSN");
        match std::env::var(&name) {
            Ok(dsn) => targets.push((Some(major), dsn)),
            Err(_) if required => return Err(format!("required {name} is missing").into()),
            Err(_) => {}
        }
    }
    if let Ok(dsn) = std::env::var("W9PT_POSTGRES_TEST_DSN") {
        targets.push((None, dsn));
    }

    let mut seen = BTreeSet::new();
    for (ordinal, (major, dsn)) in targets.into_iter().enumerate() {
        if !should_run_target(major, &dsn, &mut seen) {
            continue;
        }
        let harness = open_harness(&dsn, major, 500 + ordinal as u128).await?;
        check_state_store_conformance(&harness).await?;
    }
    Ok(())
}

#[test]
fn required_version_targets_are_never_silently_deduplicated() {
    let mut seen = BTreeSet::new();
    assert!(should_run_target(Some(15), "shared", &mut seen));
    assert!(should_run_target(Some(16), "shared", &mut seen));
    assert!(should_run_target(Some(17), "shared", &mut seen));
    assert!(should_run_target(Some(18), "shared", &mut seen));
    assert!(!should_run_target(None, "shared", &mut seen));
    assert!(should_run_target(None, "optional-only", &mut seen));
}
