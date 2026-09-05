//! PostgreSQL store construction and public API.

use sea_orm::DatabaseConnection;
#[cfg(feature = "test-support")]
use sea_orm::TransactionTrait;
use w9pt_fs_state::{
    AcquireLeaseOutcome, AcquireWriterLease, ChangePoll, ChangePollOutcome, CommitOutcome,
    CommitRequest, FilesystemStateStore, ReadBatch, ReadOutcome, ReleaseLeaseOutcome,
    ReleaseWriterLease, RenewLeaseOutcome, RenewWriterLease, StateStoreContract,
    StateStoreGuarantees, WriterTopology,
};

use crate::{
    LeaseClockSource, PostgresOpenError, PostgresStateConfig, PostgresStateError, change, commit,
    database::require_postgres_connection, lease, migration, read, validation,
};

/// Result of an explicit embedded migration run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MigrationReport {
    applied: u32,
    current_version: u32,
}

impl MigrationReport {
    /// Constructs a migration report.
    pub const fn new(applied: u32, current_version: u32) -> Self {
        Self {
            applied,
            current_version,
        }
    }

    /// Returns the number of migrations applied by this invocation.
    pub const fn applied(self) -> u32 {
        self.applied
    }

    /// Returns the current embedded schema version.
    pub const fn current_version(self) -> u32 {
        self.current_version
    }
}

/// PostgreSQL implementation of the authoritative filesystem-state contract.
#[derive(Clone, Debug)]
pub struct PostgresStateStore {
    database: DatabaseConnection,
    config: PostgresStateConfig,
    contract: StateStoreContract,
    clock: LeaseClockSource,
    #[cfg(feature = "test-support")]
    test_control: Option<crate::testing::DatabaseTestControl>,
}

impl PostgresStateStore {
    /// Opens a store over a caller-owned SeaORM connection without applying migrations.
    pub async fn open(
        database: DatabaseConnection,
        config: PostgresStateConfig,
    ) -> Result<Self, PostgresOpenError> {
        require_postgres_connection(&database).map_err(PostgresOpenError::Database)?;
        let contract = StateStoreContract::production(
            WriterTopology::SerializableMultiWriter,
            StateStoreGuarantees::PRODUCTION_REQUIRED,
            config.limits(),
        )?;
        validation::validate_open(&database, &config).await?;
        Ok(Self {
            database,
            config,
            contract,
            clock: LeaseClockSource::Database,
            #[cfg(feature = "test-support")]
            test_control: None,
        })
    }

    /// Opens a feature-gated conformance store with an explicit database clock source.
    #[cfg(feature = "test-support")]
    pub async fn open_for_testing(
        database: DatabaseConnection,
        config: PostgresStateConfig,
        clock: crate::testing::TestLeaseClockSource,
    ) -> Result<Self, PostgresOpenError> {
        let mut store = Self::open(database, config).await?;
        let clock = LeaseClockSource::from(clock);
        if let LeaseClockSource::Manual(manual) = clock {
            let mut transaction = store
                .database
                .begin()
                .await
                .map_err(PostgresOpenError::Database)?;
            manual
                .capture_in_transaction(&mut transaction)
                .await
                .map_err(|error| PostgresOpenError::Validation(error.to_string()))?;
            transaction
                .rollback()
                .await
                .map_err(PostgresOpenError::Database)?;
        }
        store.clock = clock;
        store.test_control = match clock {
            LeaseClockSource::Database => None,
            LeaseClockSource::Manual(manual) => {
                Some(crate::testing::DatabaseTestControl::for_clock(manual))
            }
        };
        Ok(store)
    }

    /// Explicitly applies all pending embedded migrations.
    ///
    /// This entry point never reads credentials or creates its own pool. Migration
    /// execution is implemented separately from [`Self::open`] so callers can use
    /// distinct migration and runtime roles.
    pub async fn migrate(
        database: &DatabaseConnection,
    ) -> Result<MigrationReport, PostgresOpenError> {
        Self::migrate_with_config(database, PostgresStateConfig::default()).await
    }

    /// Applies pending migrations with explicit bounded transaction timeouts.
    ///
    /// The state limits and retry fields are ignored by migration execution;
    /// [`PostgresStateConfig::statement_timeout`] and
    /// [`PostgresStateConfig::lock_timeout`] bound every database statement and
    /// advisory-lock wait.
    pub async fn migrate_with_config(
        database: &DatabaseConnection,
        config: PostgresStateConfig,
    ) -> Result<MigrationReport, PostgresOpenError> {
        require_postgres_connection(database).map_err(PostgresOpenError::Database)?;
        migration::migrate(database, config)
            .await
            .map_err(PostgresOpenError::Migration)
    }

    /// Returns the caller-owned SeaORM connection used by this adapter.
    pub const fn database_connection(&self) -> &DatabaseConnection {
        &self.database
    }

    /// Returns the checked adapter configuration.
    pub const fn config(&self) -> PostgresStateConfig {
        self.config
    }

    /// Returns the validated serializable multi-writer production contract.
    pub const fn contract(&self) -> StateStoreContract {
        self.contract
    }

    /// Returns the authoritative lease clock selected by construction.
    pub const fn lease_clock_source(&self) -> LeaseClockSource {
        self.clock
    }
}

impl FilesystemStateStore for PostgresStateStore {
    type Error = PostgresStateError;

    fn contract(&self) -> StateStoreContract {
        self.contract
    }

    fn read(
        &self,
        request: ReadBatch,
    ) -> impl Future<Output = Result<ReadOutcome, Self::Error>> + Send {
        read::read_request(&self.database, self.config, request)
    }

    async fn commit(&self, request: CommitRequest) -> Result<CommitOutcome, Self::Error> {
        #[cfg(feature = "test-support")]
        let injection = match self.test_control {
            Some(control) => {
                control
                    .take_commit_failure(&self.database)
                    .await
                    .map_err(|error| {
                        PostgresStateError::new(
                            w9pt_fs_state::StateStoreOperation::Commit,
                            w9pt_fs_state::AdapterFailureKind::Internal,
                            error.to_string(),
                        )
                    })?
            }
            None => None,
        };
        #[cfg(not(feature = "test-support"))]
        let injection = None;
        commit::commit_request(&self.database, self.config, self.clock, injection, request).await
    }

    fn acquire_writer_lease(
        &self,
        request: AcquireWriterLease,
    ) -> impl Future<Output = Result<AcquireLeaseOutcome, Self::Error>> + Send {
        lease::acquire_writer_lease(&self.database, self.config, self.clock, request)
    }

    fn renew_writer_lease(
        &self,
        request: RenewWriterLease,
    ) -> impl Future<Output = Result<RenewLeaseOutcome, Self::Error>> + Send {
        lease::renew_writer_lease(&self.database, self.config, self.clock, request)
    }

    fn release_writer_lease(
        &self,
        request: ReleaseWriterLease,
    ) -> impl Future<Output = Result<ReleaseLeaseOutcome, Self::Error>> + Send {
        lease::release_writer_lease(&self.database, self.config, self.clock, request)
    }

    fn poll_changes(
        &self,
        request: ChangePoll,
    ) -> impl Future<Output = Result<ChangePollOutcome, Self::Error>> + Send {
        change::poll_changes(&self.database, self.config, request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn construction_contract_is_production_multi_writer() {
        let config = PostgresStateConfig::default();
        let contract = StateStoreContract::production(
            WriterTopology::SerializableMultiWriter,
            StateStoreGuarantees::PRODUCTION_REQUIRED,
            config.limits(),
        )
        .expect("default contract is valid");
        assert_eq!(
            contract.writer_topology(),
            WriterTopology::SerializableMultiWriter
        );
        assert!(contract.is_production_ready());
    }
}
