//! Exact PostgreSQL-authoritative lease-clock capture.

use core::fmt;

use sea_orm::{ConnectionTrait, DbErr};
use w9pt_fs_state::{AdapterFailureKind, LeaseDeadline, StateStoreOperation};

use crate::{
    PostgresStateError,
    database::{PostgresTransaction, query_scalar},
    numeric::{NumericCodecError, decode_u64},
    sqlstate::SqlOperationPhase,
};

#[cfg(feature = "test-support")]
use crate::testing::{DatabaseManualClockError, TestLeaseClockSource};

/// Authoritative lease clock selected by a store instance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeaseClockSource {
    /// Capture one checked `clock_timestamp()` value per production transaction.
    Database,
    /// Capture a deterministic tick from the separate test-control schema.
    #[cfg(feature = "test-support")]
    Manual(crate::testing::DatabaseManualClock),
}

#[cfg(feature = "test-support")]
impl From<TestLeaseClockSource> for LeaseClockSource {
    fn from(source: TestLeaseClockSource) -> Self {
        match source {
            TestLeaseClockSource::Database => Self::Database,
            TestLeaseClockSource::Manual(clock) => Self::Manual(clock),
        }
    }
}

/// Version-1 defines one lease tick as one microsecond.
pub const LEASE_TICK_UNIT_MICROSECONDS: u64 = 1;

/// Number of version-1 lease ticks in one second.
pub const LEASE_TICKS_PER_SECOND: u64 = 1_000_000;

// The materialized CTE gives the volatile clock expression one evaluation site.
// PostgreSQL performs the epoch arithmetic as `numeric`, reduces the already
// microsecond-precision timestamp to scale zero, and only then transports the
// canonical decimal text. No floating-point value crosses the driver boundary.
const DATABASE_CLOCK_SQL: &str = r#"
WITH observed_clock AS MATERIALIZED (
    SELECT pg_catalog.clock_timestamp() AS observed_at
)
SELECT (
    pg_catalog.trunc(
        EXTRACT(EPOCH FROM observed_at) * 1000000::numeric
    )::numeric(20, 0)
)::text AS lease_tick
FROM observed_clock
"#;

/// Failure while obtaining or decoding one authoritative database-clock tick.
#[derive(Debug)]
pub(crate) enum DatabaseClockError {
    /// PostgreSQL or the driver failed to execute the one-observation query.
    Query(DbErr),
    /// PostgreSQL returned a negative, noncanonical, or out-of-range tick.
    InvalidTick(NumericCodecError),
}

impl fmt::Display for DatabaseClockError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Query(error) => write!(formatter, "database lease-clock query failed: {error}"),
            Self::InvalidTick(error) => write!(formatter, "invalid database lease tick: {error}"),
        }
    }
}

impl std::error::Error for DatabaseClockError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Query(error) => Some(error),
            Self::InvalidTick(error) => Some(error),
        }
    }
}

/// Captures PostgreSQL wall time exactly once and returns Unix-epoch microseconds.
///
/// The caller supplies the authoritative transaction or connection. Lease and
/// fence operations should call this once, then reuse the returned deadline for
/// every comparison and checked deadline calculation in that operation.
pub(crate) async fn capture_database_lease_deadline<C>(
    connection: &C,
) -> Result<LeaseDeadline, DatabaseClockError>
where
    C: ConnectionTrait + ?Sized,
{
    let numeric_text: String = query_scalar(DATABASE_CLOCK_SQL)
        .fetch_one(connection)
        .await
        .map_err(DatabaseClockError::Query)?;
    decode_database_tick(&numeric_text)
}

pub(crate) async fn capture_lease_deadline(
    source: LeaseClockSource,
    transaction: &mut PostgresTransaction,
    operation: StateStoreOperation,
) -> Result<LeaseDeadline, PostgresStateError> {
    match source {
        LeaseClockSource::Database => {
            capture_database_lease_deadline(transaction)
                .await
                .map_err(|error| match error {
                    DatabaseClockError::Query(error) => PostgresStateError::from_database(
                        operation,
                        SqlOperationPhase::ExecuteStatement,
                        error,
                    ),
                    DatabaseClockError::InvalidTick(error) => PostgresStateError::new(
                        operation,
                        AdapterFailureKind::Corruption,
                        error.to_string(),
                    ),
                })
        }
        #[cfg(feature = "test-support")]
        LeaseClockSource::Manual(clock) => {
            clock
                .capture_in_transaction(transaction)
                .await
                .map_err(|error| match error {
                    DatabaseManualClockError::Database(error) => PostgresStateError::from_database(
                        operation,
                        SqlOperationPhase::ExecuteStatement,
                        error,
                    ),
                    error => PostgresStateError::new(
                        operation,
                        AdapterFailureKind::Corruption,
                        error.to_string(),
                    ),
                })
        }
    }
}

fn decode_database_tick(numeric_text: &str) -> Result<LeaseDeadline, DatabaseClockError> {
    decode_u64("database_lease_tick", numeric_text)
        .map(LeaseDeadline::new)
        .map_err(DatabaseClockError::InvalidTick)
}

#[cfg(test)]
mod tests {
    use sea_orm::{ConnectOptions, Database, TransactionTrait};

    use super::*;

    #[test]
    fn version_one_tick_is_one_microsecond() {
        assert_eq!(LEASE_TICK_UNIT_MICROSECONDS, 1);
        assert_eq!(LEASE_TICKS_PER_SECOND, 1_000_000);
    }

    #[test]
    fn query_has_one_materialized_clock_observation_and_numeric_transport() {
        assert_eq!(DATABASE_CLOCK_SQL.matches("clock_timestamp()").count(), 1);
        assert!(DATABASE_CLOCK_SQL.contains("AS MATERIALIZED"));
        assert!(DATABASE_CLOCK_SQL.contains("EXTRACT(EPOCH"));
        assert!(DATABASE_CLOCK_SQL.contains("1000000::numeric"));
        assert!(DATABASE_CLOCK_SQL.contains("::numeric(20, 0)"));
        assert!(DATABASE_CLOCK_SQL.contains(")::text AS lease_tick"));
        assert!(!DATABASE_CLOCK_SQL.to_ascii_lowercase().contains("float"));
        assert!(!DATABASE_CLOCK_SQL.contains("now()"));
    }

    #[test]
    fn canonical_full_range_ticks_decode_without_signed_narrowing() {
        for tick in [0, i64::MAX as u64, i64::MAX as u64 + 1, u64::MAX] {
            assert_eq!(
                decode_database_tick(&tick.to_string()).unwrap(),
                LeaseDeadline::new(tick)
            );
        }
    }

    #[test]
    fn negative_fractional_noncanonical_and_overflow_ticks_are_rejected() {
        for tick in ["-1", "1.0", "01", "+1", " 1", "18446744073709551616"] {
            assert!(
                matches!(
                    decode_database_tick(tick),
                    Err(DatabaseClockError::InvalidTick(_))
                ),
                "accepted invalid database tick {tick:?}"
            );
        }
    }

    #[tokio::test]
    async fn live_database_clock_works_on_connection_and_transaction()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("W9PT_POSTGRES_TEST_DSN") else {
            return Ok(());
        };
        let mut options = ConnectOptions::new(dsn);
        options.max_connections(2).sqlx_logging(false);
        let database = Database::connect(options).await?;

        let connection_tick = capture_database_lease_deadline(&database).await?;
        let transaction = database.begin().await?;
        let transaction_tick = capture_database_lease_deadline(&transaction).await?;
        transaction.rollback().await?;

        assert!(connection_tick.ticks() > 0);
        assert!(transaction_tick >= connection_tick);
        Ok(())
    }
}
