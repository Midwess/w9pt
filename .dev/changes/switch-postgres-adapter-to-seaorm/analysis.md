# Analysis: Switch PostgreSQL State Adapter to SeaORM

## Current State

`w9pt-fs-state-postgres` is a completed PostgreSQL 15–18 implementation of `FilesystemStateStore`. It directly depends on SQLx 0.8.6 and exposes a caller-owned `sqlx::PgPool`. Its schema, codecs, reads, commits, leases, change polling, migrations, startup validation, deterministic test controls, and conformance harness are implemented and passing.

Direct SQLx surface is spread across twelve modules:

```text
change.rs
clock.rs
commit.rs
error.rs
lease.rs
migration.rs
read.rs
record_write.rs
store.rs
testing.rs
transaction.rs
validation.rs
```

Five integration-test files and several live unit tests also construct SQLx pools or execute SQLx statements. The semantic-only configuration, numeric, key, row-codec, and SQLSTATE policy modules are mostly independent of the driver and should remain stable.

## Version Compatibility

SeaORM 2.0 is not viable under current repository policy. Its current package metadata requires Rust 1.94 and SQLx 0.9, while the workspace remains Rust 1.85.

SeaORM 1.1.20 is the compatible line:

- declared MSRV: Rust 1.81;
- PostgreSQL driver: SQLx 0.8.x transitively;
- minimal proposed features: `sqlx-postgres`, `runtime-tokio`;
- default features disabled;
- entity macros not required for the proposed raw-statement gateway.

The crate can remove its direct SQLx edge, but cannot remove SQLx from the resolved graph because SeaORM's PostgreSQL backend uses it internally.

## Existing Patterns to Preserve

- Caller-owned database/runtime policy rather than URL or credential discovery inside the adapter.
- Fixed fully qualified `w9pt_fs_state_v1` identifiers and byte-stable migration SQL.
- Canonical numeric text conversion for the full `u64` range.
- Exact byte-preserving row codecs and cursor ordering.
- One primary `SERIALIZABLE READ ONLY` transaction per read batch.
- Ledger-first `SERIALIZABLE READ WRITE` commits with deterministic semantic locks.
- Explicit transaction phases and conservative uncertain-commit recovery.
- SQLSTATE plus named-constraint classification without message matching.
- Database-time leases, permanently retained fences, and exact operation replay.
- Bounded SQL-side scan planning before materializing large values.
- Independent client/pool and PostgreSQL 15–18 conformance testing.

## SeaORM Capabilities That Map Cleanly

| Current SQLx concept | Proposed SeaORM concept |
|---|---|
| `PgPool` | caller-created `DatabaseConnection` |
| `Transaction<Postgres>` | `DatabaseTransaction` |
| `query` / `query_scalar` | `Statement` plus `ConnectionTrait` |
| `PgRow` / `Row::try_get` | `QueryResult::try_get` |
| `PgQueryResult::rows_affected` | `ExecResult::rows_affected` |
| `BEGIN` plus `SET TRANSACTION` | `begin_with_config(Serializable, ReadOnly/ReadWrite)` |
| raw multi-statement execution | `execute_unprepared` inside an explicit transaction |
| pool clones | cloned caller-owned `DatabaseConnection` handles |

## Areas Requiring Proof, Not Assumption

### Native error fidelity

Portable `DbErr::sql_err()` is insufficient because it covers only broad uniqueness/foreign-key categories. The adapter requires exact `40001`, `40P01`, `23505`, `23503`, `23514`, `25006`, timeout, shutdown, and connection classes plus exact constraint names and operation phase.

SeaORM 1.1.20 retains SQLx-backed runtime errors under `DbErr::{Conn, Exec, Query}`. A spike must prove the adapter can extract code and constraint through SeaORM's public re-exports without importing or depending directly on SQLx. If it cannot, the migration stops.

### Commit-phase ambiguity

The adapter must distinguish an error returned by a statement from an error returned while `COMMIT` is in flight. Closure-managed transaction APIs are unsuitable. The rewrite must retain explicit `DatabaseTransaction` ownership and call `commit` itself so unknown commit errors still produce exact ambiguous recovery.

### Migration locking

The current migration runner pins one acquired PostgreSQL connection and holds a session advisory lock across per-migration transactions. An opaque `DatabaseConnection` does not expose a physical checked-out connection without leaking SQLx.

The proposed replacement uses one explicit SeaORM read-write transaction and the same advisory key through `pg_advisory_xact_lock`. It bootstraps the ledger, validates all applied migrations, executes all pending transactional migration SQL, records checksums, and commits once. This preserves serialization and strengthens all-pending atomicity, but requires every future embedded migration to remain PostgreSQL-transactional.

### Raw SQL remains normative

Entity and ActiveModel APIs do not express several correctness-bearing operations clearly enough:

- canonical multi-family `FOR UPDATE` order;
- predicate reads for absent keys;
- explicit `$n::numeric` casts;
- bounded scan CTEs and cumulative byte filters;
- database catalog/setting checks;
- change event and lease ledgers;
- exact migration SQL/checksums.

The rewrite should centralize SeaORM execution while retaining the existing SQL constants. Introducing generated entities would duplicate the portable row codecs and increase the risk of silent SQL or representation changes.

## Public API Impact

Current:

```rust
PostgresStateStore::open(pool: sqlx::PgPool, config)
PostgresStateStore::migrate(pool: &sqlx::PgPool)
store.pool() -> &sqlx::PgPool
```

Proposed:

```rust
PostgresStateStore::open(database: sea_orm::DatabaseConnection, config)
PostgresStateStore::migrate(database: &sea_orm::DatabaseConnection)
store.database_connection() -> &sea_orm::DatabaseConnection
```

This is intentionally breaking. A compatibility constructor accepting `PgPool` would retain a direct SQLx public type and violate the requested boundary. Callers that already own a SQLx pool may convert it to SeaORM outside this crate, but new callers can construct the `DatabaseConnection` directly with SeaORM `ConnectOptions`.

## Affected File Inventory

### Dependency and API

- `crates/w9pt-fs-state-postgres/Cargo.toml`
- `Cargo.lock`
- `src/lib.rs`
- `src/store.rs`
- `src/error.rs`
- new `src/database.rs`

### Database execution

- `src/transaction.rs`
- `src/migration.rs`
- `src/validation.rs`
- `src/clock.rs`
- `src/read.rs`
- `src/change.rs`
- `src/record_write.rs`
- `src/commit.rs`
- `src/lease.rs`
- `src/testing.rs`

### Tests and documentation

- `tests/bootstrap.rs`
- `tests/manual_clock.rs`
- `tests/migration.rs`
- `tests/postgres_conformance.rs`
- `tests/review_regressions.rs`
- live unit-test setup in read/lease/testing/transaction modules
- `README.md`
- `.dev/project.md`

### Files that must not change semantically

- `migrations/0001_initial.sql`
- `src/config.rs`
- `src/numeric.rs`
- `src/key_codec.rs`
- `src/row_codec.rs`
- `src/sqlstate.rs`
- all `w9pt-fs-state` public types and outcomes

## Recommended Internal Boundary

Add `src/database.rs` as the only SeaORM execution facade. It owns:

- PostgreSQL `Statement` construction from static SQL and typed values;
- query-one/query-all/execute/execute-unprepared helpers;
- `QueryResult` named-field extraction;
- serializable transaction construction;
- affected-row validation;
- adapter phase plus `DbErr` native classification.

Other modules may depend on SeaORM connection/transaction/result types where necessary, but should not reimplement error unwrapping or value conversion.

## Risks and Dependencies

- SeaORM 1.1 is a compatibility pin, not the current major line.
- Transitive SQLx patch selection is no longer controlled by an exact direct dependency for downstream consumers.
- More than one hundred query/bind sites require mechanical but correctness-sensitive conversion.
- `Statement` values may allocate more than direct SQLx binds; scan and commit benchmarks are required.
- SeaORM's error surface remains coupled to its SQLx-backed driver representation even though this crate no longer directly imports SQLx.
- Migration lock scope changes from session to transaction.
- The conversion offers abstraction consistency, not database portability; the crate remains intentionally PostgreSQL-specific.

## Readiness Conclusion

The user accepted the explicit direct-use interpretation of "no SQLx": SeaORM's PostgreSQL backend may retain SQLx transitively, while this adapter has no direct SQLx dependency or API. The feasibility gates proved native error fidelity, explicit commit-phase handling, transaction modes, value/row conversion, and migration execution, so the proposal was approved and implemented.
