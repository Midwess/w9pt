//! Typed PostgreSQL adapter failures.

use core::fmt;

use sea_orm::DbErr;
use w9pt_fs_state::{
    AdapterFailureKind, InvalidStateStoreContract, StateStoreAdapterError, StateStoreOperation,
};

use crate::{
    PostgresConfigError,
    database::classify_database_error,
    sqlstate::{SqlFailureClass, SqlOperationPhase},
};

/// Failure while opening a store or explicitly applying migrations.
#[derive(Debug)]
pub enum PostgresOpenError {
    /// Checked configuration could not form the required production contract.
    Contract(InvalidStateStoreContract),
    /// SeaORM or PostgreSQL rejected connection or transaction work.
    Database(DbErr),
    /// The fixed route, durability settings, schema, or privileges are incompatible.
    Validation(String),
    /// Checked PostgreSQL adapter configuration is invalid.
    Configuration(String),
    /// Explicit migration failed.
    Migration(MigrationError),
}

impl fmt::Display for PostgresOpenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Contract(error) => write!(formatter, "invalid state-store contract: {error}"),
            Self::Database(error) => write!(formatter, "PostgreSQL database failure: {error}"),
            Self::Validation(detail) => write!(formatter, "PostgreSQL validation failed: {detail}"),
            Self::Configuration(detail) => {
                write!(formatter, "PostgreSQL configuration failed: {detail}")
            }
            Self::Migration(error) => write!(formatter, "PostgreSQL migration failed: {error}"),
        }
    }
}

impl std::error::Error for PostgresOpenError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Contract(error) => Some(error),
            Self::Database(error) => Some(error),
            Self::Validation(_) | Self::Configuration(_) => None,
            Self::Migration(error) => Some(error),
        }
    }
}

impl From<InvalidStateStoreContract> for PostgresOpenError {
    fn from(error: InvalidStateStoreContract) -> Self {
        Self::Contract(error)
    }
}

/// Explicit migration failure.
#[derive(Debug)]
pub enum MigrationError {
    /// PostgreSQL returned an infrastructure error.
    Database(DbErr),
    /// A public relation already occupies the reserved adapter prefix without a ledger.
    PublicSchemaCollision(String),
    /// The applied migration ledger is incompatible with embedded migrations.
    Drift(String),
}

impl fmt::Display for MigrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(error) => write!(formatter, "database error: {error}"),
            Self::PublicSchemaCollision(relation) => write!(
                formatter,
                "public relation {relation} occupies the reserved w9pt_fs_state_ prefix"
            ),
            Self::Drift(detail) => write!(formatter, "migration drift: {detail}"),
        }
    }
}

impl std::error::Error for MigrationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            Self::PublicSchemaCollision(_) | Self::Drift(_) => None,
        }
    }
}

/// Definitive infrastructure failure returned by state-store operations.
#[derive(Debug)]
pub struct PostgresStateError {
    operation: StateStoreOperation,
    kind: AdapterFailureKind,
    detail: String,
    sql_failure: Option<SqlFailureClass>,
    source: Option<DbErr>,
}

impl PostgresStateError {
    pub(crate) fn new(
        operation: StateStoreOperation,
        kind: AdapterFailureKind,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            operation,
            kind,
            detail: detail.into(),
            sql_failure: None,
            source: None,
        }
    }

    pub(crate) fn from_database(
        operation: StateStoreOperation,
        phase: SqlOperationPhase,
        error: DbErr,
    ) -> Self {
        let class = classify_database_error(phase, &error);
        let kind = match class {
            SqlFailureClass::RetryIdentical(_) => AdapterFailureKind::Serialization,
            SqlFailureClass::Timeout => AdapterFailureKind::Timeout,
            SqlFailureClass::ReadOnly => AdapterFailureKind::Unsupported,
            SqlFailureClass::Unavailable | SqlFailureClass::AmbiguousCommit => {
                AdapterFailureKind::Unavailable
            }
            SqlFailureClass::Corruption => AdapterFailureKind::Corruption,
            SqlFailureClass::KnownConstraint(_) | SqlFailureClass::Internal => {
                AdapterFailureKind::Internal
            }
        };
        let detail = error.to_string();
        Self {
            operation,
            kind,
            detail,
            sql_failure: Some(class),
            source: Some(error),
        }
    }

    pub(crate) const fn sql_failure(&self) -> Option<SqlFailureClass> {
        self.sql_failure
    }

    pub(crate) const fn with_operation(mut self, operation: StateStoreOperation) -> Self {
        self.operation = operation;
        self
    }
}

impl fmt::Display for PostgresStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "PostgreSQL {:?} failure during {:?}: {}",
            self.kind, self.operation, self.detail
        )
    }
}

impl std::error::Error for PostgresStateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}

impl StateStoreAdapterError for PostgresStateError {
    fn kind(&self) -> AdapterFailureKind {
        self.kind
    }

    fn operation(&self) -> StateStoreOperation {
        self.operation
    }
}

impl From<PostgresConfigError> for PostgresOpenError {
    fn from(error: PostgresConfigError) -> Self {
        Self::Configuration(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use sea_orm::RuntimeErr;

    use super::*;

    #[test]
    fn runtime_database_failure_retains_its_source() {
        let error = PostgresStateError::from_database(
            StateStoreOperation::Read,
            SqlOperationPhase::ExecuteStatement,
            DbErr::Query(RuntimeErr::Internal("driver failure".to_owned())),
        );
        assert!(std::error::Error::source(&error).is_some());
        let error = error.with_operation(StateStoreOperation::Commit);
        assert_eq!(error.operation(), StateStoreOperation::Commit);
        assert!(error.sql_failure().is_some());
        assert!(std::error::Error::source(&error).is_some());
    }
}
