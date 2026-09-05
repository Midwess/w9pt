# Design: SeaORM Boundary for the PostgreSQL State Adapter

## 1. Definition of "No More SQLx"

The adapter crate SHALL have no direct SQLx dependency, `sqlx::` path, imported SQLx type, public SQLx type, or SQLx-based test API. SeaORM's PostgreSQL backend remains transitively SQLx-backed. This distinction is normative for this change.

A literal SQLx-free resolved dependency graph cannot use SeaORM's PostgreSQL backend and is therefore a stop condition, not an implementation task.

## 2. Version Decision

Pin SeaORM exactly 1.1.20 with defaults disabled and `sqlx-postgres` plus `runtime-tokio` enabled. This line declares Rust 1.81 and an SQLx 0.8 dependency range, fitting Rust 1.85.

Do not use SeaORM 2.x. Its current Rust 1.94 and SQLx 0.9 requirements require a separate workspace MSRV and driver upgrade decision.

## 3. SeaORM Is an Execution Facade

The adapter remains PostgreSQL-specific. SeaORM replaces direct driver plumbing but does not generate authoritative SQL or redefine state records.

Retain:

- all fixed SQL constants and explicit PostgreSQL casts;
- all row/key/numeric codecs;
- all schema and migration bytes;
- all transaction ordering and semantic validation;
- all public `w9pt-fs-state` outcomes.

Do not add entities, ActiveModels, generated schema, or ORM relationships. Those would duplicate portable domain records and obscure exact SQL required for correctness.

## 4. Caller-Owned Connection

`PostgresStateStore` accepts a caller-created `DatabaseConnection`. The caller chooses connection URL, credentials, TLS features, pool sizing, routing, and runtime. The adapter validates that the backend is PostgreSQL, clones the handle for owned async work, and never closes it implicitly.

The old `PgPool` constructor and accessor are removed. No compatibility overload is provided because naming `PgPool` would violate the direct-SQLx boundary.

## 5. Database Gateway

One private `database.rs` module owns SeaORM-specific mechanics:

- typed value conversion and statement construction;
- query/execute/unprepared calls;
- named row extraction;
- serializable transaction creation;
- affected-row checks;
- connection/backend validation;
- phase-aware `DbErr` inspection.

This keeps SeaORM coupling auditable and prevents each semantic module from inventing its own error or bind behavior.

## 6. Explicit Transactions

Use `begin_with_config` to request PostgreSQL serializable access modes. Continue verifying actual transaction settings from PostgreSQL after begin.

Transactions remain explicitly owned and completed. A closure API cannot be used because an error during `COMMIT` is materially different from a statement error and may require ledger recovery.

## 7. Native Driver Errors

The existing SQLSTATE policy remains pure. The gateway extracts native code and named constraint from SeaORM runtime errors through public SeaORM APIs/re-exports, combines them with the adapter-owned phase, and calls `sqlstate.rs`.

Do not use localized message matching or SeaORM's coarse portable `SqlErr` as the sole classifier. If exact fields cannot be recovered, the change stops.

## 8. Migrations

SeaORM does not expose a physical pool connection without revealing its SQLx backend. Replace the session advisory lock with `pg_advisory_xact_lock` inside one explicit transaction.

The transaction performs bootstrap, ledger validation, every pending embedded migration, and ledger insertion before one commit. The fixed advisory key and migration bytes stay unchanged. Future migrations must be transaction-safe.

## 9. Persistent Compatibility

This change has no SQL migration and no data rewrite. All existing databases remain readable by the old and new adapter implementations. Rollback before release is code-only.

## 10. Acceptance and Stop Rules

Implementation is accepted only when:

- Rust 1.85 builds with the locked graph;
- direct dependency/source scans find no SQLx use;
- the transitive-driver caveat is documented;
- exact SQLSTATE/constraint and commit-phase behavior are proven live;
- all existing state semantics and migration checksums remain identical;
- PostgreSQL 15–18 conformance and failure regressions pass through independent SeaORM connections;
- performance does not regress beyond an explicitly reviewed budget.

Any failed proof returns the proposal to draft rather than weakening the state-store contract.
