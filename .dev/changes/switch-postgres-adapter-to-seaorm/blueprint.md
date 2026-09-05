# Implementation Blueprint: Switch PostgreSQL State Adapter to SeaORM

## Design Approach

Convert `w9pt-fs-state-postgres` from a direct SQLx adapter into a SeaORM-backed PostgreSQL adapter without changing its filesystem semantics or persistent representation:

```text
future filesystem engine
  -> w9pt-fs-state::FilesystemStateStore
       -> w9pt-fs-state-postgres
            -> private SeaORM statement/error gateway
                 -> caller-owned sea_orm::DatabaseConnection
                      -> SeaORM's transitive SQLx PostgreSQL driver
```

SeaORM is a driver facade here, not a portability layer. All correctness-bearing PostgreSQL statements remain fixed, explicit, fully qualified, and tested.

## Dependency Shape

```toml
sea-orm = {
    version = "=1.1.20",
    default-features = false,
    features = ["sqlx-postgres", "runtime-tokio"]
}
```

Remove the direct `sqlx` dependency entirely. Do not add `sea-orm-migration`, entity code generation, SeaORM CLI, or default feature bundles. Tokio may remain a test-only dependency for async test entry points.

Acceptance checks:

```text
cargo tree -p w9pt-fs-state-postgres --depth 1
  contains sea-orm
  does not contain sqlx

rg '(^|[^a-zA-Z_])sqlx::|use sqlx' crates/w9pt-fs-state-postgres
  returns no matches
```

The unrestricted dependency tree will still contain SQLx below SeaORM and must document that fact.

## Public API

```rust
pub struct PostgresStateStore {
    database: sea_orm::DatabaseConnection,
    config: PostgresStateConfig,
    contract: StateStoreContract,
    clock: LeaseClockSource,
    // feature-gated test controls unchanged semantically
}

impl PostgresStateStore {
    pub async fn open(
        database: sea_orm::DatabaseConnection,
        config: PostgresStateConfig,
    ) -> Result<Self, PostgresOpenError>;

    pub async fn migrate(
        database: &sea_orm::DatabaseConnection,
    ) -> Result<MigrationReport, PostgresOpenError>;

    pub fn database_connection(&self) -> &sea_orm::DatabaseConnection;
}
```

`open` verifies `DbBackend::Postgres` before catalog or state work. It does not read a URL, construct a pool, select TLS, or run migrations. The embedding application creates `DatabaseConnection` with its own `ConnectOptions` and feature-unified TLS choice.

## Private Database Gateway

Create `src/database.rs` with a small, audited surface:

```text
postgres_statement(sql, values) -> Statement
execute(connection, phase, statement) -> ExecResult
query_one(connection, phase, statement) -> Option<QueryResult>
query_all(connection, phase, statement) -> Vec<QueryResult>
execute_unprepared(connection, phase, sql)
begin_serializable(database, access) -> DatabaseTransaction
get<T>(row, field) -> T
classify_db_err(operation, phase, DbErr) -> PostgresStateError / retry action
```

Use `Statement::from_sql_and_values(DbBackend::Postgres, sql, values)` for parameterized statements. Keep adapter-owned phase tags; SeaORM's `DbErr` alone does not identify whether a failure occurred during statement execution, rollback, or commit.

The gateway must bound result materialization using the same SQL limits and preflight CTEs as the existing implementation. It must not turn an unbounded query into `query_all` merely because SeaORM exposes that helper.

## Row and Value Mapping

Keep the current primitive row structs in `row_codec.rs`. Replace `PgRow` decoding with named `QueryResult::try_get` operations and map failures to corruption during authoritative decode phases.

Bind values using SeaORM/SeaQuery value variants:

- fixed IDs, digests, names, and values as byte vectors;
- canonical numeric text as strings followed by existing explicit `::numeric` casts;
- signed seconds and integer tags as signed integer values;
- booleans as booleans;
- nullable content, range, and cursor fields as typed optional values.

Do not switch persisted `NUMERIC(20,0)` fields to `BigDecimal`, floating point, signed narrowing, or ORM-generated column conversions.

## Transactions

Use `TransactionTrait::begin_with_config` with:

```text
IsolationLevel::Serializable
AccessMode::ReadOnly  for reads and change polls
AccessMode::ReadWrite for commits and leases
```

After begin, retain the current transaction-local timeout setup, primary/recovery checks, actual transaction-mode validation, and `synchronous_commit=on` enforcement.

Use owned `DatabaseTransaction` values and explicit `commit`/`rollback`. Do not use closure-managed transactions because the adapter must classify errors by phase and recover uncertain commit status through a fresh connection and ledger probe.

## Native Error Classification

Map SeaORM errors centrally:

```text
DbErr::ConnectionAcquire
DbErr::{Conn, Exec, Query}(RuntimeErr::SqlxError(...))
other DbErr variants
```

The feasibility spike must prove exact access to SQLSTATE and constraint name through SeaORM's public error re-exports. Feed those values into the existing pure `sqlstate.rs` classifier. `DbErr::sql_err()` alone is not sufficient.

Unknown statement errors remain internal/availability failures according to phase. Unknown errors returned by explicit `DatabaseTransaction::commit` remain potentially committed and enter exact ledger recovery.

## Read and Change Flow

Preserve current SQL and sequence:

1. Revalidate receiver limits.
2. Begin primary serializable read-only transaction.
3. Read private head or revision-one baseline.
4. Execute point queries in request order through SeaORM statements.
5. Run metadata-only bounded scan preflight.
6. Fetch only selected full rows and decode through current codecs.
7. Construct public snapshots/pages defensively.
8. Commit the read transaction before returning.

Change polling retains numeric keyset order, whole-event bounds, future/compacted outcomes, and `next/current_revision` semantics.

## Commit and Lease Flow

Preserve the implemented protocol order exactly:

```text
ledger probe
  -> receiver preflight
  -> serializable write transaction
  -> repeated ledger lookup
  -> authority head lock
  -> writer fence lock
  -> one clock capture
  -> canonical semantic locks/predicates
  -> typed preconditions and targeted invariants
  -> changes/result/event/revision
  -> explicit commit
  -> exact recovery if commit status is uncertain
```

Record writes use SeaORM `ExecResult::rows_affected`. Named uniqueness races must retain their native error details until the retry layer selects ledger resolution or semantic recheck.

Lease operations retain their shared operation-ID namespace, exact result rows, permanent greatest fencing tokens, success-only change events, and bounded retries.

## Migration Protocol

Keep `migrations/0001_initial.sql`, its bytes, version, checksum algorithm, fixed schema, and advisory-lock key unchanged.

Replace session locking with:

```text
1. Begin one explicit SeaORM read-write transaction.
2. SELECT pg_advisory_xact_lock(fixed_key).
3. Create schema and migration ledger if absent.
4. Read and validate every applied version/checksum.
5. Execute every pending embedded migration using execute_unprepared.
6. Insert each ledger row after its SQL succeeds.
7. Commit once.
```

All embedded migrations must remain valid inside a PostgreSQL transaction. `open` continues to fail closed and never migrates.

## Test-Support Conversion

- Replace test-control `PgPool` parameters with `DatabaseConnection`.
- Keep the separate `w9pt_fs_state_test_v1` schema.
- Preserve database-resident manual clocks and commit-failure controls.
- Construct separate SeaORM connections for writer, observer, control, and isolated-limit clients.
- Keep cleanup scoped to reserved conformance identities.

## Files to Create or Modify

```text
Cargo.toml
Cargo.lock
README.md
.dev/project.md

crates/w9pt-fs-state-postgres/
  Cargo.toml
  src/
    lib.rs
    database.rs          # new SeaORM gateway
    store.rs
    error.rs
    transaction.rs
    migration.rs
    validation.rs
    clock.rs
    read.rs
    change.rs
    record_write.rs
    commit.rs
    lease.rs
    testing.rs
  tests/
    seaorm_feasibility.rs # new retained gate coverage
    bootstrap.rs
    manual_clock.rs
    migration.rs
    postgres_conformance.rs
    review_regressions.rs
```

`config.rs`, `numeric.rs`, `key_codec.rs`, `row_codec.rs`, `sqlstate.rs`, and `migrations/0001_initial.sql` should change only when a compile-time integration detail requires it; no semantic or persistent change is allowed.

## Implementation Phases

1. Prove SeaORM 1.1.20/MSRV, statement, row, transaction, native-error, commit-phase, and migration feasibility while SQLx still exists temporarily.
2. Add the centralized SeaORM gateway and focused differential tests.
3. Change the public connection API and startup validation.
4. Convert migrations, clocks, test controls, reads, and polling.
5. Convert record writes, leases, and commits, preserving exact retry/ambiguity behavior.
6. Convert every integration/live test and independently constructed connection.
7. Remove direct SQLx completely and run dependency/source enforcement.
8. Run full PostgreSQL 15–18 conformance, performance comparison, documentation, and workspace gates.

## Testing Strategy

- Retain every existing offline codec, configuration, SQLSTATE, scan, migration, and regression test.
- Add a retained feasibility test for every SeaORM bind/result/error/transaction gate.
- Differentially compare direct-SQLx baseline results captured before removal with SeaORM results where practical.
- Run concurrent migration runners and checksum drift tests.
- Run the documented one-DSN suite and required PostgreSQL 15–18 matrix.
- Exercise known aborts, named constraints, read-only routing, timeouts, lost acknowledgments, and recovery exhaustion.
- Reopen through fresh independently created SeaORM connections with no shared adapter cache.
- Benchmark representative point-read, bounded-scan, lease, and multi-record commit paths; document material regressions.
- Run Rust 1.85 locked build, workspace tests, formatting, Clippy/rustdoc with warnings denied, and dependency/source checks.

## Stop Conditions

Stop implementation and return the proposal to draft if SeaORM cannot preserve exact native error details, explicit commit-phase ambiguity, transaction access/isolation, bounded scan behavior, byte/numeric round trips, or migration serialization without a direct SQLx dependency or semantic weakening.
