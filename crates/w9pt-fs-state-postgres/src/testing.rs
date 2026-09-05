//! Feature-gated PostgreSQL controls for deterministic adapter conformance.
//!
//! This module is intended to be exposed only by the crate's `test-support`
//! feature. It stores manual time in a separate database schema and never
//! changes production migrations or relies on process-local shared state.

use core::fmt;

use sea_orm::{DbErr, TransactionTrait};
use w9pt_fs_state::{FilesystemId, LeaseDeadline, testing::CommitFailureTiming};

use crate::{
    clock::capture_database_lease_deadline,
    database::{PostgresConnection, PostgresTransaction, query, query_scalar, unprepared_sql},
    numeric::{decode_u64, encode_u64},
};

/// Fixed schema containing test-only database controls.
pub const TEST_SCHEMA: &str = "w9pt_fs_state_test_v1";

const INSTALL_SQL: &str = r#"
CREATE SCHEMA IF NOT EXISTS "w9pt_fs_state_test_v1";
CREATE TABLE IF NOT EXISTS "w9pt_fs_state_test_v1"."lease_clocks" (
    "clock_id" bytea PRIMARY KEY,
    "now_tick" numeric(20, 0) NOT NULL,
    CONSTRAINT "lease_clocks_clock_id_width" CHECK (octet_length("clock_id") = 16),
    CONSTRAINT "lease_clocks_now_tick_range" CHECK (
        "now_tick" BETWEEN 0 AND 18446744073709551615
    )
);
CREATE TABLE IF NOT EXISTS "w9pt_fs_state_test_v1"."commit_failures" (
    "control_id" bytea PRIMARY KEY,
    "timing" smallint NOT NULL,
    CONSTRAINT "commit_failures_control_id_width" CHECK (octet_length("control_id") = 16),
    CONSTRAINT "commit_failures_timing_tag" CHECK ("timing" IN (1, 2))
);
"#;

const INITIALIZE_SQL: &str = r#"
INSERT INTO "w9pt_fs_state_test_v1"."lease_clocks" ("clock_id", "now_tick")
VALUES ($1, $2::numeric)
"#;

const CAPTURE_SQL: &str = r#"
SELECT "now_tick"::text
FROM "w9pt_fs_state_test_v1"."lease_clocks"
WHERE "clock_id" = $1
"#;

const LOCK_CLOCK_SQL: &str = r#"
SELECT "now_tick"::text
FROM "w9pt_fs_state_test_v1"."lease_clocks"
WHERE "clock_id" = $1
FOR UPDATE
"#;

const UPDATE_CLOCK_SQL: &str = r#"
UPDATE "w9pt_fs_state_test_v1"."lease_clocks"
SET "now_tick" = $2::numeric
WHERE "clock_id" = $1
"#;

const INJECT_COMMIT_FAILURE_SQL: &str = r#"
INSERT INTO "w9pt_fs_state_test_v1"."commit_failures" ("control_id", "timing")
VALUES ($1, $2)
"#;

const TAKE_COMMIT_FAILURE_SQL: &str = r#"
DELETE FROM "w9pt_fs_state_test_v1"."commit_failures"
WHERE "control_id" = $1
RETURNING "timing"
"#;

/// Stable identity of one database-resident manual clock.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DatabaseManualClockId([u8; 16]);

impl DatabaseManualClockId {
    /// Creates an identity from its exact 16-byte database representation.
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Creates an identity from a big-endian integer.
    pub const fn from_u128(value: u128) -> Self {
        Self(value.to_be_bytes())
    }

    /// Returns the exact bytes persisted in PostgreSQL.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl fmt::Debug for DatabaseManualClockId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DatabaseManualClockId(")?;
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        formatter.write_str(")")
    }
}

/// Handle to one test-only integer clock stored in PostgreSQL.
///
/// The handle owns no pool or clock state. Independently constructed handles
/// and independent pools observe the same row selected by [`Self::id`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DatabaseManualClock {
    id: DatabaseManualClockId,
}

impl DatabaseManualClock {
    /// Explicitly installs the separate test-control schema and clock table.
    ///
    /// Callers must use a role authorized for test DDL. Production migration and
    /// open paths never invoke this method.
    pub async fn install(pool: &PostgresConnection) -> Result<(), DatabaseManualClockError> {
        let transaction = pool
            .begin()
            .await
            .map_err(DatabaseManualClockError::Database)?;
        unprepared_sql(INSTALL_SQL)
            .execute(&transaction)
            .await
            .map_err(DatabaseManualClockError::Database)?;
        transaction
            .commit()
            .await
            .map_err(DatabaseManualClockError::Database)
    }

    /// Inserts one new clock row at an explicit initial tick.
    ///
    /// Initialization deliberately is not an upsert: reusing a clock identity is
    /// an error rather than an implicit reset visible to another test client.
    pub async fn initialize(
        pool: &PostgresConnection,
        id: DatabaseManualClockId,
        initial: LeaseDeadline,
    ) -> Result<Self, DatabaseManualClockError> {
        let result = query(INITIALIZE_SQL)
            .bind(id.as_bytes().to_vec())
            .bind(encode_u64(initial.ticks()))
            .execute(pool)
            .await
            .map_err(DatabaseManualClockError::Database)?;
        require_one_row("initialize manual lease clock", result.rows_affected())?;
        Ok(Self { id })
    }

    /// Constructs a handle for an already initialized clock row.
    pub const fn from_id(id: DatabaseManualClockId) -> Self {
        Self { id }
    }

    /// Returns this clock's stable database identity.
    pub const fn id(self) -> DatabaseManualClockId {
        self.id
    }

    /// Captures the current manual tick inside the caller's state transaction.
    ///
    /// Lease/fence code must call this on the same transaction that reads or
    /// changes authoritative lease state, then reuse the returned tick.
    pub async fn capture_in_transaction(
        self,
        transaction: &mut PostgresTransaction,
    ) -> Result<LeaseDeadline, DatabaseManualClockError> {
        let value = query_scalar::<String>(CAPTURE_SQL)
            .bind(self.id.as_bytes().to_vec())
            .fetch_optional(transaction)
            .await
            .map_err(DatabaseManualClockError::Database)?
            .ok_or(DatabaseManualClockError::MissingClock(self.id))?;
        decode_tick(&value)
    }

    /// Atomically advances this database-resident clock by an exact tick count.
    ///
    /// The row is locked before checked Rust arithmetic, so independent pools
    /// cannot lose increments or wrap the public `u64` domain.
    pub async fn advance_time(
        self,
        pool: &PostgresConnection,
        ticks: u64,
    ) -> Result<LeaseDeadline, DatabaseManualClockError> {
        let transaction = pool
            .begin()
            .await
            .map_err(DatabaseManualClockError::Database)?;
        let current = query_scalar::<String>(LOCK_CLOCK_SQL)
            .bind(self.id.as_bytes().to_vec())
            .fetch_optional(&transaction)
            .await
            .map_err(DatabaseManualClockError::Database)?
            .ok_or(DatabaseManualClockError::MissingClock(self.id))?;
        let current = decode_tick(&current)?;
        let next = checked_advance(current, ticks)?;
        let result = query(UPDATE_CLOCK_SQL)
            .bind(self.id.as_bytes().to_vec())
            .bind(encode_u64(next.ticks()))
            .execute(&transaction)
            .await
            .map_err(DatabaseManualClockError::Database)?;
        require_one_row("advance manual lease clock", result.rows_affected())?;
        transaction
            .commit()
            .await
            .map_err(DatabaseManualClockError::Database)?;
        Ok(next)
    }
}

/// Database-resident deterministic fault and compaction controls for conformance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DatabaseTestControl {
    id: DatabaseManualClockId,
}

impl DatabaseTestControl {
    /// Selects test controls sharing the same portable identity as a manual clock.
    pub const fn for_clock(clock: DatabaseManualClock) -> Self {
        Self { id: clock.id() }
    }

    /// Inserts one deterministic commit failure to be consumed by the next commit.
    pub async fn inject_commit_failure(
        self,
        pool: &PostgresConnection,
        timing: CommitFailureTiming,
    ) -> Result<(), DatabaseManualClockError> {
        let tag = match timing {
            CommitFailureTiming::BeforePublication => 1i16,
            CommitFailureTiming::AfterPublication => 2i16,
        };
        let result = query(INJECT_COMMIT_FAILURE_SQL)
            .bind(self.id.as_bytes().to_vec())
            .bind(tag)
            .execute(pool)
            .await
            .map_err(DatabaseManualClockError::Database)?;
        require_one_row("inject commit failure", result.rows_affected())
    }

    pub(crate) async fn take_commit_failure(
        self,
        pool: &PostgresConnection,
    ) -> Result<Option<CommitFailureTiming>, DatabaseManualClockError> {
        let tag: Option<i16> = query_scalar(TAKE_COMMIT_FAILURE_SQL)
            .bind(self.id.as_bytes().to_vec())
            .fetch_optional(pool)
            .await
            .map_err(DatabaseManualClockError::Database)?;
        tag.map(|tag| match tag {
            1 => Ok(CommitFailureTiming::BeforePublication),
            2 => Ok(CommitFailureTiming::AfterPublication),
            tag => Err(DatabaseManualClockError::InvalidFailureTiming(tag)),
        })
        .transpose()
    }

    /// Compacts all test-authority change events and advances each retained cursor.
    pub async fn compact_change_history(
        self,
        pool: &PostgresConnection,
        filesystem_ids: &[FilesystemId],
    ) -> Result<(), DatabaseManualClockError> {
        let mut filesystem_ids = filesystem_ids.to_vec();
        filesystem_ids.sort();
        filesystem_ids.dedup();
        let transaction = pool
            .begin()
            .await
            .map_err(DatabaseManualClockError::Database)?;
        for filesystem_id in filesystem_ids {
            let filesystem = filesystem_id.as_bytes().to_vec();
            query(
                r#"SELECT 1 FROM "public"."w9pt_fs_state_authority_heads"
                   WHERE "filesystem_id" = $1 FOR UPDATE"#,
            )
            .bind(filesystem.clone())
            .fetch_optional(&transaction)
            .await
            .map_err(DatabaseManualClockError::Database)?;
            query(
                r#"DELETE FROM "public"."w9pt_fs_state_change_keys"
                   WHERE "filesystem_id" = $1"#,
            )
            .bind(filesystem.clone())
            .execute(&transaction)
            .await
            .map_err(DatabaseManualClockError::Database)?;
            query(
                r#"DELETE FROM "public"."w9pt_fs_state_change_commits"
                   WHERE "filesystem_id" = $1"#,
            )
            .bind(filesystem.clone())
            .execute(&transaction)
            .await
            .map_err(DatabaseManualClockError::Database)?;
            query(
                r#"UPDATE "public"."w9pt_fs_state_authority_heads"
                   SET "oldest_retained_revision" = "current_revision"
                   WHERE "filesystem_id" = $1"#,
            )
            .bind(filesystem)
            .execute(&transaction)
            .await
            .map_err(DatabaseManualClockError::Database)?;
        }
        transaction
            .commit()
            .await
            .map_err(DatabaseManualClockError::Database)
    }
}

/// Clock source selectable by a feature-gated test store constructor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TestLeaseClockSource {
    /// Use the production PostgreSQL wall-clock query.
    Database,
    /// Use one deterministic database-resident test clock.
    Manual(DatabaseManualClock),
}

impl TestLeaseClockSource {
    /// Captures exactly one tick inside the supplied authoritative transaction.
    pub async fn capture_in_transaction(
        self,
        transaction: &mut PostgresTransaction,
    ) -> Result<LeaseDeadline, DatabaseManualClockError> {
        match self {
            Self::Database => capture_database_lease_deadline(transaction)
                .await
                .map_err(|error| DatabaseManualClockError::ProductionClock(error.to_string())),
            Self::Manual(clock) => clock.capture_in_transaction(transaction).await,
        }
    }
}

/// Failure while installing, controlling, or reading a test database clock.
#[derive(Debug)]
pub enum DatabaseManualClockError {
    /// PostgreSQL or SQLx rejected the explicit test-control operation.
    Database(DbErr),
    /// No clock row exists for the selected identity.
    MissingClock(DatabaseManualClockId),
    /// PostgreSQL returned a noncanonical or out-of-range unsigned tick.
    InvalidTick {
        /// Rejected database text representation.
        value: String,
    },
    /// Advancing the clock would exceed `u64::MAX`.
    TickOverflow {
        /// Tick locked from PostgreSQL.
        current: u64,
        /// Requested increment.
        increment: u64,
    },
    /// A point update affected an impossible number of rows.
    UnexpectedAffectedRows {
        /// Stable control operation.
        operation: &'static str,
        /// Row count reported by PostgreSQL.
        rows: u64,
    },
    /// The production clock branch failed while used by a test selector.
    ProductionClock(String),
    /// A test-control row contained an unknown failure timing tag.
    InvalidFailureTiming(i16),
}

impl fmt::Display for DatabaseManualClockError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(error) => write!(formatter, "test clock database error: {error}"),
            Self::MissingClock(id) => write!(formatter, "manual database clock {id:?} is absent"),
            Self::InvalidTick { value } => {
                write!(formatter, "invalid manual database clock tick {value:?}")
            }
            Self::TickOverflow { current, increment } => write!(
                formatter,
                "manual database clock overflow: {current} + {increment}"
            ),
            Self::UnexpectedAffectedRows { operation, rows } => {
                write!(formatter, "{operation} affected {rows} rows, expected one")
            }
            Self::ProductionClock(detail) => {
                write!(formatter, "production database clock failed: {detail}")
            }
            Self::InvalidFailureTiming(tag) => {
                write!(formatter, "invalid test commit-failure timing tag {tag}")
            }
        }
    }
}

impl std::error::Error for DatabaseManualClockError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            _ => None,
        }
    }
}

fn decode_tick(value: &str) -> Result<LeaseDeadline, DatabaseManualClockError> {
    decode_u64("test_database_lease_tick", value)
        .map(LeaseDeadline::new)
        .map_err(|_| DatabaseManualClockError::InvalidTick {
            value: value.to_owned(),
        })
}

fn checked_advance(
    current: LeaseDeadline,
    increment: u64,
) -> Result<LeaseDeadline, DatabaseManualClockError> {
    current
        .ticks()
        .checked_add(increment)
        .map(LeaseDeadline::new)
        .ok_or(DatabaseManualClockError::TickOverflow {
            current: current.ticks(),
            increment,
        })
}

fn require_one_row(operation: &'static str, rows: u64) -> Result<(), DatabaseManualClockError> {
    if rows == 1 {
        Ok(())
    } else {
        Err(DatabaseManualClockError::UnexpectedAffectedRows { operation, rows })
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use sea_orm::{ConnectOptions, Database};

    use super::*;

    #[test]
    fn clock_ids_are_fixed_and_portable() {
        let id = DatabaseManualClockId::from_u128(1);
        assert_eq!(id.as_bytes().len(), 16);
        assert_eq!(DatabaseManualClockId::new(*id.as_bytes()), id);
        assert_eq!(
            format!("{id:?}"),
            "DatabaseManualClockId(00000000000000000000000000000001)"
        );
        assert_eq!(DatabaseManualClock::from_id(id).id(), id);
    }

    #[test]
    fn install_schema_is_separate_logged_and_full_range() {
        assert_eq!(TEST_SCHEMA, "w9pt_fs_state_test_v1");
        assert!(INSTALL_SQL.contains("CREATE SCHEMA IF NOT EXISTS \"w9pt_fs_state_test_v1\""));
        assert!(INSTALL_SQL.contains("\"lease_clocks\""));
        assert!(INSTALL_SQL.contains("octet_length(\"clock_id\") = 16"));
        assert!(INSTALL_SQL.contains("numeric(20, 0)"));
        assert!(INSTALL_SQL.contains("18446744073709551615"));
        assert!(!INSTALL_SQL.contains("\"public\".\"w9pt_fs_state_"));
        assert!(!INSTALL_SQL.to_ascii_uppercase().contains("TEMP"));
        assert!(!INSTALL_SQL.to_ascii_uppercase().contains("UNLOGGED"));
    }

    #[test]
    fn every_runtime_query_is_fully_qualified_and_uses_numeric_text() {
        for query in [
            INITIALIZE_SQL,
            CAPTURE_SQL,
            LOCK_CLOCK_SQL,
            UPDATE_CLOCK_SQL,
        ] {
            assert!(query.contains("\"w9pt_fs_state_test_v1\".\"lease_clocks\""));
            assert!(!query.contains("clock_timestamp"));
        }
        assert!(INITIALIZE_SQL.contains("$2::numeric"));
        assert!(CAPTURE_SQL.contains("\"now_tick\"::text"));
        assert!(LOCK_CLOCK_SQL.contains("FOR UPDATE"));
        assert!(UPDATE_CLOCK_SQL.contains("$2::numeric"));
    }

    #[test]
    fn ticks_cover_the_complete_unsigned_domain_and_never_wrap() {
        for tick in [0, i64::MAX as u64, i64::MAX as u64 + 1, u64::MAX] {
            assert_eq!(decode_tick(&encode_u64(tick)).unwrap().ticks(), tick);
        }
        assert_eq!(
            checked_advance(LeaseDeadline::new(u64::MAX - 1), 1)
                .unwrap()
                .ticks(),
            u64::MAX
        );
        assert!(matches!(
            checked_advance(LeaseDeadline::new(u64::MAX), 1),
            Err(DatabaseManualClockError::TickOverflow { .. })
        ));
        for invalid in ["", "01", "-1", "1.0", "18446744073709551616"] {
            assert!(matches!(
                decode_tick(invalid),
                Err(DatabaseManualClockError::InvalidTick { .. })
            ));
        }
    }

    #[test]
    fn affected_row_check_is_exact() {
        assert!(require_one_row("test", 1).is_ok());
        assert!(require_one_row("test", 0).is_err());
        assert!(require_one_row("test", 2).is_err());
    }

    #[tokio::test]
    async fn independent_pools_share_the_database_clock_row()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("W9PT_POSTGRES_TEST_DSN") else {
            return Ok(());
        };
        let mut first_options = ConnectOptions::new(dsn.clone());
        first_options.max_connections(2).sqlx_logging(false);
        let first_pool = Database::connect(first_options).await?;
        let mut second_options = ConnectOptions::new(dsn);
        second_options.max_connections(2).sqlx_logging(false);
        let second_pool = Database::connect(second_options).await?;
        DatabaseManualClock::install(&first_pool).await?;

        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
            ^ u128::from(std::process::id());
        let id = DatabaseManualClockId::from_u128(nonce);
        let clock = DatabaseManualClock::initialize(&first_pool, id, LeaseDeadline::new(7)).await?;
        assert_eq!(clock.advance_time(&first_pool, 5).await?.ticks(), 12);

        let mut transaction = second_pool.begin().await?;
        let independently_opened = DatabaseManualClock::from_id(id);
        assert_eq!(
            independently_opened
                .capture_in_transaction(&mut transaction)
                .await?
                .ticks(),
            12
        );
        transaction.rollback().await?;

        query(r#"DELETE FROM "w9pt_fs_state_test_v1"."lease_clocks" WHERE "clock_id" = $1"#)
            .bind(id.as_bytes().to_vec())
            .execute(&first_pool)
            .await?;
        Ok(())
    }
}
