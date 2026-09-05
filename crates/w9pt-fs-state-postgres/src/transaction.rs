//! Common serializable transaction setup and primary-route fencing.

use w9pt_fs_state::{AdapterFailureKind, StateStoreOperation};

use crate::{
    PostgresStateConfig, PostgresStateError,
    database::{
        PostgresConnection, PostgresTransaction, begin_serializable_transaction, query,
        query_scalar, set_transaction_local_timeout,
    },
    sqlstate::SqlOperationPhase,
};

pub(crate) use crate::database::TransactionAccess;

/// Begins and validates one primary-only serializable transaction.
pub(crate) async fn begin_transaction(
    database: &PostgresConnection,
    config: PostgresStateConfig,
    access: TransactionAccess,
    operation: StateStoreOperation,
) -> Result<PostgresTransaction, PostgresStateError> {
    let transaction = begin_serializable_transaction(database, access)
        .await
        .map_err(|error| {
            PostgresStateError::from_database(
                operation,
                SqlOperationPhase::AcquireConnection,
                error,
            )
        })?;

    set_local_timeout(
        &transaction,
        "statement_timeout",
        config.statement_timeout().as_millis(),
        operation,
    )
    .await?;
    set_local_timeout(
        &transaction,
        "lock_timeout",
        config.lock_timeout().as_millis(),
        operation,
    )
    .await?;
    if access == TransactionAccess::ReadWrite {
        let _: String =
            query_scalar("SELECT pg_catalog.set_config('synchronous_commit', 'on', true)")
                .fetch_one(&transaction)
                .await
                .map_err(|error| {
                    PostgresStateError::from_database(
                        operation,
                        SqlOperationPhase::BeginTransaction,
                        error,
                    )
                })?;
    }

    validate_primary_transaction(&transaction, access, operation).await?;
    Ok(transaction)
}

/// Completes a read-only authoritative transaction.
pub(crate) async fn commit_read_transaction(
    transaction: PostgresTransaction,
    operation: StateStoreOperation,
) -> Result<(), PostgresStateError> {
    transaction.commit().await.map_err(|error| {
        PostgresStateError::from_database(operation, SqlOperationPhase::Commit, error)
    })
}

async fn set_local_timeout(
    transaction: &PostgresTransaction,
    setting: &'static str,
    milliseconds: u128,
    operation: StateStoreOperation,
) -> Result<(), PostgresStateError> {
    let milliseconds = u64::try_from(milliseconds).map_err(|_| {
        PostgresStateError::new(
            operation,
            AdapterFailureKind::Internal,
            "transaction timeout does not fit u64 milliseconds",
        )
    })?;
    set_transaction_local_timeout(
        transaction,
        setting,
        std::time::Duration::from_millis(milliseconds),
    )
    .await
    .map_err(|error| {
        PostgresStateError::from_database(operation, SqlOperationPhase::BeginTransaction, error)
    })?;
    Ok(())
}

async fn validate_primary_transaction(
    transaction: &PostgresTransaction,
    access: TransactionAccess,
    operation: StateStoreOperation,
) -> Result<(), PostgresStateError> {
    let row = query(
        r#"SELECT
               pg_catalog.pg_is_in_recovery() AS in_recovery,
               current_setting('default_transaction_read_only')::boolean AS default_read_only,
               current_setting('transaction_read_only')::boolean AS transaction_read_only,
               current_setting('transaction_isolation') AS transaction_isolation,
               current_setting('synchronous_commit') AS synchronous_commit"#,
    )
    .fetch_one(transaction)
    .await
    .map_err(|error| {
        PostgresStateError::from_database(operation, SqlOperationPhase::BeginTransaction, error)
    })?;
    let in_recovery: bool = row.try_get("in_recovery").map_err(|error| {
        PostgresStateError::from_database(operation, SqlOperationPhase::DecodeRow, error)
    })?;
    let default_read_only: bool = row.try_get("default_read_only").map_err(|error| {
        PostgresStateError::from_database(operation, SqlOperationPhase::DecodeRow, error)
    })?;
    let transaction_read_only: bool = row.try_get("transaction_read_only").map_err(|error| {
        PostgresStateError::from_database(operation, SqlOperationPhase::DecodeRow, error)
    })?;
    let isolation: String = row.try_get("transaction_isolation").map_err(|error| {
        PostgresStateError::from_database(operation, SqlOperationPhase::DecodeRow, error)
    })?;
    let synchronous_commit: String = row.try_get("synchronous_commit").map_err(|error| {
        PostgresStateError::from_database(operation, SqlOperationPhase::DecodeRow, error)
    })?;

    let expected_read_only = access == TransactionAccess::ReadOnly;
    if in_recovery
        || default_read_only
        || transaction_read_only != expected_read_only
        || isolation != "serializable"
        || (access == TransactionAccess::ReadWrite && synchronous_commit != "on")
    {
        return Err(PostgresStateError::new(
            operation,
            AdapterFailureKind::Unsupported,
            "transaction is not a validated serializable writable-primary route",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectOptions, Database};

    #[test]
    fn transaction_modes_are_distinct() {
        assert_ne!(TransactionAccess::ReadOnly, TransactionAccess::ReadWrite);
    }

    #[tokio::test]
    async fn live_primary_accepts_both_serializable_modes() -> Result<(), Box<dyn std::error::Error>>
    {
        let Ok(dsn) = std::env::var("W9PT_POSTGRES_TEST_DSN") else {
            return Ok(());
        };
        let mut options = ConnectOptions::new(dsn);
        options.max_connections(2).sqlx_logging(false);
        let database = Database::connect(options).await?;
        let config = PostgresStateConfig::default();
        let read = begin_transaction(
            &database,
            config,
            TransactionAccess::ReadOnly,
            StateStoreOperation::Read,
        )
        .await?;
        commit_read_transaction(read, StateStoreOperation::Read).await?;
        let write = begin_transaction(
            &database,
            config,
            TransactionAccess::ReadWrite,
            StateStoreOperation::Commit,
        )
        .await?;
        write.rollback().await?;
        Ok(())
    }
}
