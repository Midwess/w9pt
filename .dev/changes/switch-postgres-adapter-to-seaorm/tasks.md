# Tasks: Switch PostgreSQL State Adapter to SeaORM

## Progress: [41/41]

### 1. Mandatory feasibility gates

- [x] 1.1 Add exact SeaORM `=1.1.20` with default features disabled and `sqlx-postgres`/`runtime-tokio` alongside the existing driver temporarily; prove the locked graph builds on Rust 1.85 and record the transitive SQLx caveat.
- [x] 1.2 Prove caller-created PostgreSQL `DatabaseConnection` values reject other backends and independently constructed connections use independent pools without adapter-owned URLs or credentials.
- [x] 1.3 Prove `begin_with_config` supplies `SERIALIZABLE READ ONLY` and `SERIALIZABLE READ WRITE`, retains one transaction connection, permits transaction-local settings, and exposes explicit commit/rollback failures.
- [x] 1.4 Prove `Statement`, SeaORM values, `QueryResult`, and `ExecResult` losslessly cover every current byte, string, boolean, signed integer, nullable field, numeric-text cast, row decode, and rows-affected shape.
- [x] 1.5 Prove SeaORM public errors expose exact SQLSTATE and constraint names for serialization, deadlock, unique, foreign-key, check, read-only, timeout, pool, connection, and shutdown failures without direct SQLx imports or message matching.
- [x] 1.6 Prove a deferred constraint or injected transport error returned by explicit `DatabaseTransaction::commit` remains distinguishable from pre-commit errors and supports exact ambiguous-commit recovery.
- [x] 1.7 Prove `execute_unprepared` can apply the unchanged embedded migration under `pg_advisory_xact_lock`; test fresh, repeated, concurrent, gap, newer-version, and checksum-drift cases.
- [x] 1.8 Record the feasibility results. Stop and return the proposal to draft if any gate requires direct SQLx use, loses native error/phase information, weakens bounds, or changes persistent SQL.

### 2. SeaORM gateway and public connection API

- [x] 2.1 Add `src/database.rs` with centralized PostgreSQL `Statement` construction, typed values, query/execute/unprepared helpers, named row extraction, affected-row validation, and adapter phase handling.
- [x] 2.2 Replace `PostgresStateStore`'s `PgPool` field and `open`/`pool` API with caller-owned `DatabaseConnection`, `open`, and `database_connection`; reject non-PostgreSQL or disconnected connections explicitly.
- [x] 2.3 Replace SQLx-bearing migration, open, runtime, and test-control error variants with `DbErr`-based errors while retaining `StateStoreAdapterError` categories and sources.
- [x] 2.4 Convert common transaction setup to `DatabaseTransaction` and SeaORM isolation/access configuration while preserving local timeouts, primary checks, actual-mode checks, and `synchronous_commit=on`.
- [x] 2.5 Convert startup version, schema/checksum, privilege, table-persistence, primary, and durability validation through the SeaORM gateway.
- [x] 2.6 Add caller-side SeaORM connection helpers in tests using `ConnectOptions`; keep DSN, TLS, pool sizing, and runtime lifecycle outside the adapter.

### 3. Rows, reads, polling, and record persistence

- [x] 3.1 Replace `PgRow`/SQLx decode bounds with `QueryResult` named decoding while preserving every primitive row struct, optional `ContentRef`, full-range numeric, and corruption check.
- [x] 3.2 Convert private-head revision reads and all finalized point queries to SeaORM statements without changing query order or revision-one semantics.
- [x] 3.3 Convert all ten metadata-first bounded keyset scans, preserving exact SQL, cursor order, lookahead, retained-byte accounting, and no oversized payload materialization.
- [x] 3.4 Convert change header/key queries and exact bounded polling outcomes, preserving numeric revision order and whole-event semantics.
- [x] 3.5 Convert production clock capture and feature-gated database manual clocks to SeaORM transactions and values without changing microsecond ticks.
- [x] 3.6 Convert canonical record locks, inserts, replacements, deletes, mutation insertion, and affected-row checks without ORM-generated SQL or hidden upserts.

### 4. Commits, leases, retries, and ambiguity

- [x] 4.1 Convert acquire/renew/release operation ledgers, fence rows, revision publication, retries, and exact replay to SeaORM while preserving permanent greatest tokens.
- [x] 4.2 Convert the short schema-bound mutation ledger probe and repeat it first inside each write attempt before current limits or fences.
- [x] 4.3 Convert authority-head/fence/canonical semantic locking, absent predicates, every typed precondition, staged lock conflicts, and targeted dependency validation.
- [x] 4.4 Convert every `StateChange`, dedicated content/xattr publication, exact revision stamping, mutation result insertion, event publication, and monotonic compaction.
- [x] 4.5 Route SeaORM statement errors through the existing SQLSTATE/constraint policy and preserve exact identical retries for `40001`, `40P01`, and recognized constraint races.
- [x] 4.6 Preserve explicit commit-phase classification, fresh-connection ledger probes, exact resubmission, bounded recovery attempts, and `CommitOutcome::Ambiguous` exhaustion.
- [x] 4.7 Differentially compare representative direct-SQLx baseline reads, commits, leases, polls, semantic rejections, and retained results with the SeaORM implementation before removing the old path.

### 5. Migrations, test controls, and direct-SQLx removal

- [x] 5.1 Replace the session advisory-lock runner with one SeaORM transaction using the unchanged advisory key and `pg_advisory_xact_lock`; keep `0001_initial.sql` byte-for-byte unchanged.
- [x] 5.2 Test concurrent migration runners, transactional rollback, idempotency, checksum drift, version gaps, unknown newer versions, and compatibility with an already migrated database.
- [x] 5.3 Convert `w9pt_fs_state_test_v1` installation, manual clock, failure injection, scoped cleanup, and scoped compaction APIs to caller-owned `DatabaseConnection`.
- [x] 5.4 Convert bootstrap, manual-clock, migration, conformance, and review-regression integration tests from SQLx pools/statements to SeaORM connections/statements.
- [x] 5.5 Preconstruct independent SeaORM connections for writer/observer clients and create fresh SeaORM connections for isolated-limit clients; prove no correctness-bearing shared adapter state.
- [x] 5.6 Re-run before/post-publication failure injection, tighter-limit replay, constraint race, lock replacement, history-limit, stale fence, and fresh-client recovery regressions.
- [x] 5.7 Remove the direct SQLx dependency and every direct SQLx path/type from production code, test code, public API, and examples; retain only SeaORM's documented transitive driver.

### 6. Verification and documentation

- [x] 6.1 Add dependency/source enforcement showing SeaORM is direct, SQLx is not a depth-one dependency, and `sqlx::`/`use sqlx` do not occur anywhere in the adapter crate.
- [x] 6.2 Run every offline configuration, numeric, row/key codec, SQLSTATE, scan, migration, and query-shape test under default and `test-support` features.
- [x] 6.3 Run the documented full single-DSN suite on a dedicated PostgreSQL database and prove scoped cleanup permits repeat runs.
- [x] 6.4 Run the reusable conformance and recovery suite through independent SeaORM connections on current PostgreSQL 15, 16, 17, and 18 minors in required mode.
- [x] 6.5 Benchmark representative point reads, bounded scans, lease operations, and multi-record commits against the direct-SQLx baseline; document and approve any material regression.
- [x] 6.6 Update README and `.dev/project.md` for SeaORM construction, caller-owned connection policy, TLS/pool features, the transitive SQLx caveat, migration transaction locking, and test commands.
- [x] 6.7 Run Rust 1.85 locked build, workspace tests, formatting, Clippy with warnings denied, rustdoc with warnings denied, dependency checks, and verify progress is `41/41`.

---

## Notes

- This proposal removes direct SQLx use; SeaORM's PostgreSQL backend remains transitively SQLx-backed.
- SeaORM 2.x and SQLx 0.9 require a separate MSRV upgrade proposal.
- Raw fully qualified PostgreSQL statements remain normative; this is not an entity/ActiveModel rewrite.
- The production schema and `0001_initial.sql` must not change.
- Feasibility passed on PostgreSQL 16 under the Rust 1.85 locked graph: 10/10 live gates covered caller-owned independent pools, PostgreSQL/backend rejection, serializable access modes, transaction-local settings and connection retention, primitive binds/decodes/affected rows, exact native SQLSTATE/constraint extraction, commit/rollback failures, and transaction-scoped advisory migration locking.
- Native details were preserved for serialization (`40001`), deadlock (`40P01`), unique (`23505`), foreign-key (`23503`), check (`23514`), read-only (`25006`), and timeout/cancellation (`57014`) failures; pool, connection, and shutdown failures retained typed SeaORM/driver variants. Commit-phase transport loss remains separately classifiable as ambiguous.
- The unchanged embedded migration passed fresh, repeated, concurrent, gap, unknown-newer, and checksum-drift checks through `execute_unprepared`. Its recorded SHA-256 before implementation rewiring is `1ba1d7fb880eeb1de330366a0f7ca4158ea5f1890bb16309d63f1a5027af5d84`.
- Neither `src/database.rs` nor `tests/seaorm_feasibility.rs` contains a direct `sqlx::` path or `use sqlx`; implementation may proceed.
- The retained direct-SQLx baseline conformance/review outcomes were replayed through the SeaORM path on PostgreSQL 16: all reusable conformance scenarios and the reviewed commit-edge regression passed with the same exact public outcomes for reads, commits, leases, polls, semantic rejections, failure recovery, and retained results.
- A temporary release-mode matched-driver benchmark on local PostgreSQL 16 alternated five 100-operation samples per serializable transaction shape. SeaORM/direct-SQLx median ratios were point read `0.866`, bounded scan `1.031`, lease-like six-statement transaction `1.059`, and commit-like twelve-statement transaction `1.023`. The largest observed regression was 5.9%, below the reviewed 10% material-regression budget; the temporary direct dependency and harness were removed after measurement.
- Post-implementation review fixes force bounded `READ COMMITTED READ WRITE` migrations regardless of caller session defaults, bound migration-ledger lookahead and startup validation, preserve commit-side `DbErr` sources/classification, reject false-green required matrix deduplication, and enforce the actual depth-one dependency graph. The reproduced concurrent migration regression under caller-default `REPEATABLE READ`, default-read-only migration, and bounded advisory-lock wait all pass on PostgreSQL 16.
