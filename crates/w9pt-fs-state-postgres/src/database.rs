//! SeaORM access gateway for fixed PostgreSQL statements and transactions.

use core::marker::PhantomData;
use std::time::Duration;

use sea_orm::{
    AccessMode, ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, DbErr,
    ExecResult, IsolationLevel, QueryResult, Statement, TransactionTrait, TryGetable, Value,
    error::{ConnAcquireErr, RuntimeErr, SqlxError},
};

use crate::sqlstate::{SqlFailureClass, SqlOperationPhase, classify_sqlstate};

/// Caller-owned PostgreSQL connection type used throughout the adapter.
pub(crate) type PostgresConnection = DatabaseConnection;

/// One connection-retaining SeaORM database transaction.
pub(crate) type PostgresTransaction = DatabaseTransaction;

/// Rejects disconnected or non-PostgreSQL SeaORM connections at construction.
pub(crate) fn require_postgres_connection(database: &DatabaseConnection) -> Result<(), DbErr> {
    if matches!(database, DatabaseConnection::SqlxPostgresPoolConnection(_)) {
        Ok(())
    } else {
        Err(DbErr::Conn(RuntimeErr::Internal(
            "a connected PostgreSQL SeaORM DatabaseConnection is required".to_owned(),
        )))
    }
}

/// One named SeaORM result row.
pub(crate) struct PostgresRow(QueryResult);

impl PostgresRow {
    /// Decodes a named column from this row.
    pub(crate) fn try_get<T>(&self, column: &str) -> Result<T, DbErr>
    where
        T: TryGetable,
    {
        get(&self.0, column)
    }
}

/// A fixed PostgreSQL statement with incrementally collected SeaORM values.
pub(crate) struct PostgresQuery {
    sql: String,
    values: Vec<Value>,
}

impl PostgresQuery {
    /// Appends one positional PostgreSQL bind value.
    pub(crate) fn bind(mut self, value: impl Into<Value>) -> Self {
        self.values.push(value.into());
        self
    }

    fn statement(self) -> Statement {
        postgres_statement_with_values(self.sql, self.values)
    }

    /// Executes this statement and returns its affected-row result.
    pub(crate) async fn execute<C>(self, connection: &C) -> Result<ExecResult, DbErr>
    where
        C: ConnectionTrait + ?Sized,
    {
        execute(connection, self.statement()).await
    }

    /// Executes this query and requires exactly one result row.
    pub(crate) async fn fetch_one<C>(self, connection: &C) -> Result<PostgresRow, DbErr>
    where
        C: ConnectionTrait + ?Sized,
    {
        query_one(connection, self.statement())
            .await?
            .map(PostgresRow)
            .ok_or_else(|| DbErr::RecordNotFound("query returned no row".to_owned()))
    }

    /// Executes this query and returns an optional row.
    pub(crate) async fn fetch_optional<C>(
        self,
        connection: &C,
    ) -> Result<Option<PostgresRow>, DbErr>
    where
        C: ConnectionTrait + ?Sized,
    {
        Ok(query_one(connection, self.statement())
            .await?
            .map(PostgresRow))
    }

    /// Executes this explicitly bounded query and returns all result rows.
    pub(crate) async fn fetch_all<C>(self, connection: &C) -> Result<Vec<PostgresRow>, DbErr>
    where
        C: ConnectionTrait + ?Sized,
    {
        Ok(query_all(connection, self.statement())
            .await?
            .into_iter()
            .map(PostgresRow)
            .collect())
    }
}

/// A fixed query whose first selected column is decoded as one scalar.
pub(crate) struct PostgresScalarQuery<T> {
    query: PostgresQuery,
    value: PhantomData<fn() -> T>,
}

impl<T> PostgresScalarQuery<T> {
    /// Appends one positional PostgreSQL bind value.
    pub(crate) fn bind(mut self, value: impl Into<Value>) -> Self {
        self.query = self.query.bind(value);
        self
    }
}

impl<T> PostgresScalarQuery<T>
where
    T: TryGetable,
{
    /// Executes this query and requires one first-column scalar.
    pub(crate) async fn fetch_one<C>(self, connection: &C) -> Result<T, DbErr>
    where
        C: ConnectionTrait + ?Sized,
    {
        let row = query_one(connection, self.query.statement())
            .await?
            .ok_or_else(|| DbErr::RecordNotFound("scalar query returned no row".to_owned()))?;
        row.try_get_by_index(0)
    }

    /// Executes this query and decodes an optional first-column scalar.
    pub(crate) async fn fetch_optional<C>(self, connection: &C) -> Result<Option<T>, DbErr>
    where
        C: ConnectionTrait + ?Sized,
    {
        query_one(connection, self.query.statement())
            .await?
            .map(|row| row.try_get_by_index(0))
            .transpose()
    }
}

/// Trusted embedded SQL executed without prepared-statement parsing.
#[cfg(feature = "test-support")]
pub(crate) struct PostgresUnpreparedSql<'a>(&'a str);

#[cfg(feature = "test-support")]
impl PostgresUnpreparedSql<'_> {
    /// Executes this trusted SQL string as one unprepared batch.
    pub(crate) async fn execute<C>(self, connection: &C) -> Result<ExecResult, DbErr>
    where
        C: ConnectionTrait + ?Sized,
    {
        execute_unprepared(connection, self.0).await
    }
}

/// Starts construction of one parameterized PostgreSQL statement.
pub(crate) fn query(sql: impl Into<String>) -> PostgresQuery {
    PostgresQuery {
        sql: sql.into(),
        values: Vec::new(),
    }
}

/// Starts construction of one first-column scalar PostgreSQL query.
pub(crate) fn query_scalar<T>(sql: impl Into<String>) -> PostgresScalarQuery<T> {
    PostgresScalarQuery {
        query: query(sql),
        value: PhantomData,
    }
}

/// Wraps trusted embedded SQL for unprepared execution.
#[cfg(feature = "test-support")]
pub(crate) const fn unprepared_sql(sql: &str) -> PostgresUnpreparedSql<'_> {
    PostgresUnpreparedSql(sql)
}

/// Authoritative transaction access mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TransactionAccess {
    /// One-snapshot state read or change poll.
    ReadOnly,
    /// Atomic state, lease, or migration transition.
    ReadWrite,
}

impl TransactionAccess {
    const fn sea_orm(self) -> AccessMode {
        match self {
            Self::ReadOnly => AccessMode::ReadOnly,
            Self::ReadWrite => AccessMode::ReadWrite,
        }
    }
}

/// Owned, locale-independent details from a native PostgreSQL error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NativeSqlError {
    sqlstate: String,
    constraint: Option<String>,
}

impl NativeSqlError {
    /// Returns the five-character PostgreSQL SQLSTATE.
    pub(crate) fn sqlstate(&self) -> &str {
        &self.sqlstate
    }

    /// Returns the exact PostgreSQL constraint name, when supplied.
    pub(crate) fn constraint(&self) -> Option<&str> {
        self.constraint.as_deref()
    }
}

/// Builds one static or dynamically assembled PostgreSQL statement without values.
#[cfg(test)]
pub(crate) fn postgres_statement(sql: impl Into<String>) -> Statement {
    Statement::from_string(DbBackend::Postgres, sql)
}

/// Builds one parameterized PostgreSQL statement from SeaORM values.
pub(crate) fn postgres_statement_with_values<I>(sql: impl Into<String>, values: I) -> Statement
where
    I: IntoIterator<Item = Value>,
{
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

/// Executes a prepared statement through a SeaORM connection or transaction.
pub(crate) async fn execute<C>(connection: &C, statement: Statement) -> Result<ExecResult, DbErr>
where
    C: ConnectionTrait + ?Sized,
{
    connection.execute(statement).await
}

/// Executes trusted embedded SQL that does not contain caller-controlled text.
#[cfg(feature = "test-support")]
pub(crate) async fn execute_unprepared<C>(connection: &C, sql: &str) -> Result<ExecResult, DbErr>
where
    C: ConnectionTrait + ?Sized,
{
    connection.execute_unprepared(sql).await
}

/// Executes a query that may return at most one row.
pub(crate) async fn query_one<C>(
    connection: &C,
    statement: Statement,
) -> Result<Option<QueryResult>, DbErr>
where
    C: ConnectionTrait + ?Sized,
{
    connection.query_one(statement).await
}

/// Executes a query whose SQL supplies an explicit adapter-owned row bound.
pub(crate) async fn query_all<C>(
    connection: &C,
    statement: Statement,
) -> Result<Vec<QueryResult>, DbErr>
where
    C: ConnectionTrait + ?Sized,
{
    connection.query_all(statement).await
}

/// Decodes one named result column through SeaORM's PostgreSQL row mapping.
pub(crate) fn get<T>(row: &QueryResult, column: &str) -> Result<T, DbErr>
where
    T: TryGetable,
{
    row.try_get("", column)
}

/// Returns the exact affected-row count reported by PostgreSQL.
pub(crate) fn rows_affected(result: &ExecResult) -> u64 {
    result.rows_affected()
}

/// Validates a statement's affected-row count against an explicit inclusive range.
pub(crate) fn validate_rows_affected(
    result: &ExecResult,
    allowed: core::ops::RangeInclusive<u64>,
) -> Result<u64, u64> {
    let actual = rows_affected(result);
    if allowed.contains(&actual) {
        Ok(actual)
    } else {
        Err(actual)
    }
}

/// Begins one explicit serializable SeaORM transaction.
///
/// Callers commit or roll back the returned transaction themselves so an error
/// while `COMMIT` is in flight remains distinguishable from statement errors.
pub(crate) async fn begin_serializable_transaction(
    database: &DatabaseConnection,
    access: TransactionAccess,
) -> Result<DatabaseTransaction, DbErr> {
    database
        .begin_with_config(Some(IsolationLevel::Serializable), Some(access.sea_orm()))
        .await
}

/// Begins one explicit read-committed read-write transaction.
pub(crate) async fn begin_read_committed_write_transaction(
    database: &DatabaseConnection,
) -> Result<DatabaseTransaction, DbErr> {
    database
        .begin_with_config(
            Some(IsolationLevel::ReadCommitted),
            Some(AccessMode::ReadWrite),
        )
        .await
}

/// Applies one bounded PostgreSQL timeout to the current transaction.
pub(crate) async fn set_transaction_local_timeout(
    transaction: &DatabaseTransaction,
    setting: &'static str,
    timeout: Duration,
) -> Result<(), DbErr> {
    let sql = match setting {
        "statement_timeout" => "SELECT pg_catalog.set_config('statement_timeout', $1, true)",
        "lock_timeout" => "SELECT pg_catalog.set_config('lock_timeout', $1, true)",
        _ => {
            return Err(DbErr::Custom(
                "unknown transaction-local timeout setting".to_owned(),
            ));
        }
    };
    let value = format!("{}ms", timeout.as_millis());
    let _: String = query_scalar(sql).bind(value).fetch_one(transaction).await?;
    Ok(())
}

/// Extracts native SQLSTATE and constraint details retained by SeaORM.
///
/// SeaORM deliberately keeps its native driver error as the source of `Conn`,
/// `Exec`, and `Query` failures. This adapter accesses that public re-export so
/// it never needs a direct dependency on, import from, or public type from the
/// underlying driver crate.
pub(crate) fn native_sql_error(error: &DbErr) -> Option<NativeSqlError> {
    let database = driver_error(error)?.as_database_error()?;
    Some(NativeSqlError {
        sqlstate: database.code()?.into_owned(),
        constraint: database.constraint().map(str::to_owned),
    })
}

/// Classifies a SeaORM failure using its operation phase and native details.
pub(crate) fn classify_database_error(phase: SqlOperationPhase, error: &DbErr) -> SqlFailureClass {
    if let Some(native) = native_sql_error(error) {
        return classify_sqlstate(phase, native.sqlstate(), native.constraint());
    }

    if let Some(error) = driver_error(error) {
        return match error {
            SqlxError::PoolTimedOut => commit_or(phase, SqlFailureClass::Timeout),
            SqlxError::Io(_)
            | SqlxError::Tls(_)
            | SqlxError::PoolClosed
            | SqlxError::WorkerCrashed => commit_or(phase, SqlFailureClass::Unavailable),
            SqlxError::ColumnDecode { .. } | SqlxError::Decode(_) => SqlFailureClass::Corruption,
            _ if phase == SqlOperationPhase::DecodeRow => SqlFailureClass::Corruption,
            _ => commit_or(phase, SqlFailureClass::Internal),
        };
    }

    match error {
        DbErr::ConnectionAcquire(ConnAcquireErr::Timeout) => {
            commit_or(phase, SqlFailureClass::Timeout)
        }
        DbErr::ConnectionAcquire(ConnAcquireErr::ConnectionClosed) | DbErr::Conn(_) => {
            commit_or(phase, SqlFailureClass::Unavailable)
        }
        _ if phase == SqlOperationPhase::DecodeRow => SqlFailureClass::Corruption,
        _ => commit_or(phase, SqlFailureClass::Internal),
    }
}

fn driver_error(error: &DbErr) -> Option<&SqlxError> {
    match error {
        DbErr::Conn(RuntimeErr::SqlxError(error))
        | DbErr::Exec(RuntimeErr::SqlxError(error))
        | DbErr::Query(RuntimeErr::SqlxError(error)) => Some(error),
        _ => None,
    }
}

fn commit_or(phase: SqlOperationPhase, otherwise: SqlFailureClass) -> SqlFailureClass {
    if phase == SqlOperationPhase::Commit {
        SqlFailureClass::AmbiguousCommit
    } else {
        otherwise
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statements_are_fixed_to_the_postgres_backend() {
        let plain = postgres_statement("SELECT 1");
        assert_eq!(plain.db_backend, DbBackend::Postgres);
        assert_eq!(plain.sql, "SELECT 1");
        assert!(plain.values.is_none());

        let parameterized = postgres_statement_with_values(
            "SELECT $1::integer, $2::text, $3::bytea",
            vec![
                7i32.into(),
                "value".to_owned().into(),
                vec![1u8, 2, 3].into(),
            ],
        );
        assert_eq!(parameterized.db_backend, DbBackend::Postgres);
        assert_eq!(parameterized.values.expect("values").0.len(), 3);
    }

    #[test]
    fn disconnected_connection_is_rejected() {
        assert!(require_postgres_connection(&DatabaseConnection::default()).is_err());
    }

    #[test]
    fn transaction_access_maps_without_a_default_mode() {
        assert_eq!(TransactionAccess::ReadOnly.sea_orm(), AccessMode::ReadOnly);
        assert_eq!(
            TransactionAccess::ReadWrite.sea_orm(),
            AccessMode::ReadWrite
        );
    }

    #[test]
    fn phase_classification_preserves_commit_ambiguity() {
        let error = DbErr::Query(RuntimeErr::SqlxError(SqlxError::Protocol(
            "lost response".to_owned(),
        )));
        assert_eq!(
            classify_database_error(SqlOperationPhase::ExecuteStatement, &error),
            SqlFailureClass::Internal
        );
        assert_eq!(
            classify_database_error(SqlOperationPhase::Commit, &error),
            SqlFailureClass::AmbiguousCommit
        );
    }

    #[test]
    fn pool_and_decode_failures_keep_their_semantic_classes() {
        let timeout = DbErr::ConnectionAcquire(ConnAcquireErr::Timeout);
        assert_eq!(
            classify_database_error(SqlOperationPhase::AcquireConnection, &timeout),
            SqlFailureClass::Timeout
        );
        assert_eq!(
            classify_database_error(SqlOperationPhase::Commit, &timeout),
            SqlFailureClass::AmbiguousCommit
        );

        let decode = DbErr::Type("invalid authoritative column".to_owned());
        assert_eq!(
            classify_database_error(SqlOperationPhase::DecodeRow, &decode),
            SqlFailureClass::Corruption
        );
    }

    #[test]
    fn non_database_driver_errors_have_no_native_details() {
        let error = DbErr::Query(RuntimeErr::SqlxError(SqlxError::Protocol(
            "invalid frame".to_owned(),
        )));
        assert_eq!(native_sql_error(&error), None);
    }
}
