# Switch PostgreSQL State Adapter to SeaORM

Status: approved

## Summary

Replace every direct SQLx dependency, import, public type, query call, row type, transaction type, and test use in `w9pt-fs-state-postgres` with SeaORM 1.1.20 APIs. The adapter will accept a caller-created `sea_orm::DatabaseConnection`, execute its existing fully qualified PostgreSQL statements through SeaORM `Statement` and `ConnectionTrait`, and preserve the complete finalized filesystem-state contract, schema bytes, transaction order, failure outcomes, durability validation, and PostgreSQL 15–18 conformance.

This proposal interprets "no more SQLx" as no direct `sqlx` dependency or `sqlx::` API in the adapter crate, its tests, or its public interface. SeaORM's PostgreSQL backend is itself implemented on SQLx, so SQLx remains a transitive implementation dependency. A literal requirement that SQLx disappear from `Cargo.lock` is incompatible with using SeaORM for PostgreSQL and would block approval of this proposal.

## Motivation

Using SeaORM as the database facade can give the PostgreSQL adapter one consistent connection, transaction, statement, row, and error surface while removing direct driver types from its public API. It also makes caller construction use the same `DatabaseConnection` abstraction that future SeaORM-based application components may already own.

The change is intentionally not an entity/ActiveModel rewrite. The authoritative adapter depends on exact PostgreSQL SQL for deterministic record locking, serializable predicate reads, SQL-side scan byte planning, full-range numeric casts, catalog validation, named constraints, database clocks, and ambiguous commit recovery. Those statements remain explicit and independently testable; SeaORM replaces the direct execution plumbing rather than the filesystem transaction design.

## Assumption

- "No more SQLx" means no direct dependency edge, imports, paths, public types, or test APIs from the `sqlx` crate in `w9pt-fs-state-postgres`.
- SeaORM's transitive SQLx-backed PostgreSQL driver is permitted and documented.
- If either assumption is rejected, this proposal is infeasible as written and must not be approved.

## Goals

- Pin SeaORM exactly `=1.1.20` with default features disabled and only the PostgreSQL/Tokio features required by the adapter.
- Preserve the workspace Rust 1.85 baseline; do not adopt SeaORM 2.x while it requires a newer compiler and SQLx 0.9.
- Remove the direct SQLx dependency from normal and development dependencies.
- Eliminate every `sqlx::` path and directly imported SQLx type from adapter source and tests.
- Replace `PgPool` construction APIs with caller-created `sea_orm::DatabaseConnection` APIs.
- Keep DSNs, credentials, TLS features, pool sizing, routing, and runtime lifecycle caller-owned.
- Preserve all production SQL text, schema names, tables, constraints, indexes, migration bytes, migration checksums, and stored representations.
- Preserve one-snapshot serializable reads, ledger-first commits, deterministic locks, exact SQLSTATE/constraint classification, ambiguous commit recovery, database-time leases, fencing, and bounded change polling.
- Run the existing offline, live, failure, and conformance suites through independently constructed SeaORM connections on PostgreSQL 15–18.

## Scope

### In scope

- Exact SeaORM 1.1.20 dependency selection and minimal feature configuration.
- A private SeaORM database gateway for parameterized statements, raw execution, row decoding, transactions, affected-row checks, and phase-aware errors.
- Public API migration from `PgPool` to `DatabaseConnection` for store construction, migration, accessors, and test controls.
- Conversion of every direct SQLx query, row, transaction, pool, and error use in the adapter and tests.
- Explicit PostgreSQL backend validation before authoritative operations.
- Transaction-scoped advisory locking for explicit migrations through one outer SeaORM transaction.
- Preservation of raw fully qualified SQL where SeaORM entities/query builders cannot express the required semantics transparently.
- Dependency and source checks proving the adapter has no direct SQLx use.
- Differential and conformance testing proving no semantic or persistent-format drift.

### Out of scope

- Removing SQLx from SeaORM's transitive dependency graph.
- Raising the workspace MSRV to adopt SeaORM 2.x or SQLx 0.9.
- Introducing SeaORM entities, ActiveModels, schema sync, generated migrations, or SeaORM CLI requirements.
- Changing `w9pt-fs-state`, `w9pt-fs-storage`, or `w9pt` public APIs or dependencies.
- Changing `w9pt_fs_state_v1`, `0001_initial.sql`, checksums, record encodings, revision semantics, or durability promises.
- Adding support for another database, replica reads, multi-primary routing, content storage, or session durability.
- Weakening exact SQLSTATE, constraint, transaction-phase, retry, fencing, or ambiguous-commit behavior when SeaORM abstractions are insufficient.

## Affected Areas

| Area | Impact |
|---|---|
| `crates/w9pt-fs-state-postgres/Cargo.toml` / `Cargo.lock` | Replace direct SQLx with exact SeaORM 1.1.20 and verify Rust 1.85 resolution |
| `src/database.rs` | New centralized SeaORM statement, row, transaction, and native-error gateway |
| `src/store.rs`, `transaction.rs`, `error.rs` | Replace public/internal pool and transaction APIs |
| `src/migration.rs`, `validation.rs`, `clock.rs` | Convert raw execution and database inspection; change migration lock lifetime |
| `src/read.rs`, `change.rs`, `record_write.rs` | Convert row/bind/execution plumbing without changing SQL semantics |
| `src/commit.rs`, `lease.rs`, `testing.rs` | Preserve retries, ambiguity, fencing, and deterministic test controls through SeaORM |
| PostgreSQL integration tests | Replace `PgPoolOptions` and raw SQLx calls with caller-created SeaORM connections |
| `README.md`, `.dev/project.md` | Document the new public connection boundary and transitive-driver caveat |
| `migrations/0001_initial.sql` | No change permitted |

## Dependencies

- Completed `add-postgres-state-adapter` implementation and its passing PostgreSQL 15–18 conformance suite.
- SeaORM exactly 1.1.20, default features disabled, with `sqlx-postgres` and `runtime-tokio`.
- Rust 1.85 and Rust 2024 workspace compatibility.
- SeaORM public APIs for `DatabaseConnection`, `DatabaseTransaction`, `Statement`, `ConnectionTrait`, `TransactionTrait`, `QueryResult`, `ExecResult`, `DbErr`, and runtime driver-error inspection.
- Caller-created PostgreSQL SeaORM connections using caller-selected TLS and pool policy.

## Mandatory Feasibility Gate

Before broad conversion, a focused spike must prove all of the following:

1. Exact SeaORM 1.1.20 and its locked transitive graph compile under Rust 1.85 with minimal features.
2. A caller-created PostgreSQL `DatabaseConnection` supports independent pools and explicit serializable read-only/read-write transactions.
3. SeaORM statements losslessly bind every current byte, string, boolean, signed integer, nullable, and numeric-text value.
4. `QueryResult` losslessly decodes every current row shape and preserves bounded scan behavior.
5. SeaORM's public error surface exposes SQLSTATE and named constraints without message matching or a direct SQLx import.
6. Errors from `DatabaseTransaction::commit` retain enough information to distinguish definitive aborts from ambiguous commit status.
7. Multi-statement embedded migration SQL runs transactionally under `pg_advisory_xact_lock` and preserves exact checksums.
8. Current PostgreSQL 15–18 conformance, failure injection, tighter-limit replay, and independent-client recovery remain expressible.

Failure of any gate blocks the change. Implementation must not restore a direct SQLx dependency, parse localized messages, hide commit-phase ambiguity, weaken transaction isolation, or change the persistent schema to work around a SeaORM limitation.

## Risks

| Risk | Mitigation |
|---|---|
| SeaORM 2.x breaks the MSRV and driver pin | Pin 1.1.20 exactly; require a separate future MSRV/SeaORM upgrade proposal |
| "No SQLx" is interpreted as no transitive dependency | State the accepted direct-use definition explicitly; block approval if literal removal is required |
| `DbErr` hides native PostgreSQL detail | Make native SQLSTATE/constraint extraction a stop/go spike before conversion |
| SeaORM transaction APIs blur commit versus statement failures | Use explicit `DatabaseTransaction::commit`/`rollback` and preserve adapter-owned phase tags |
| `QueryResult` changes byte or numeric behavior | Differentially round-trip every persisted row and full-range numeric boundary |
| SeaORM values cannot express an existing bind shape | Gate all heterogeneous/nullable bind forms before removing SQLx |
| Transaction-scoped migration locking changes runner behavior | Apply all pending transactional migrations in one outer transaction and test concurrent runners/drift |
| ORM abstractions silently alter SQL | Retain static fully qualified SQL; do not introduce entity-generated authoritative statements |
| Rewrite adds allocations and abstraction without performance benefit | Benchmark representative read/commit/scan paths and record the regression budget |
| Downstream Cargo resolution selects another SQLx 0.8 patch transitively | Pin SeaORM, commit the workspace lockfile, test Rust 1.85, and document that exact downstream SQLx pinning is no longer an adapter guarantee |

## Rollback

The production schema and persisted values do not change. Before release, rollback is a Rust dependency/code revert with no DDL or data migration. Old direct-SQLx and new SeaORM-backed binaries may target the same migrated database, subject to the existing writer fencing and deployment rules.

## Verified References

- SeaORM 1.1.20 declares Rust 1.81 and SQLx 0.8.4-compatible dependencies: <https://raw.githubusercontent.com/SeaQL/sea-orm/1.1.20/Cargo.toml>
- SeaORM 2.0.2 currently declares Rust 1.94 and SQLx 0.9: <https://github.com/SeaQL/sea-orm/blob/master/Cargo.toml>
- SeaORM can wrap or own PostgreSQL connection pools: <https://docs.rs/sea-orm/latest/sea_orm/struct.SqlxPostgresConnector.html>
- SeaORM exposes configurable transaction isolation and access modes: <https://www.sea-ql.org/SeaORM/docs/advanced-query/transaction/>
- SeaORM raw statements support SQL plus bound values: <https://docs.rs/sea-orm/1.1.20/sea_orm/struct.Statement.html>
- SeaORM `DbErr` retains runtime query/execute/connection errors: <https://docs.rs/sea-orm/1.1.20/sea_orm/error/enum.DbErr.html>
