# Analysis: Add PostgreSQL State Adapter

## Current State

- The workspace currently contains `w9pt` and `w9pt-storage`.
- `w9pt` is a dependency-free Sans-I/O protocol/session core and must remain free of database clients and runtimes.
- `w9pt-storage` prepares immutable content and portable `ContentRef`/`PreparedContent` values. It does not own authoritative inode or namespace state.
- `add-filesystem-state-store` is a draft proposal at `0/37`; `crates/w9pt-fs-state` and its `FilesystemStateStore` trait do not yet exist.
- The PostgreSQL adapter can be designed now, but implementation is blocked until the trait's IDs, records, limits, outcomes, lease time contract, and conformance factory are stable.
- Current workspace conventions are Rust 2024, Rust 1.85, resolver 3, Apache-2.0, missing-documentation warnings, `unsafe_code = "forbid"`, and Clippy warnings.
- The current Git history does not isolate the Rust scaffold; existing user changes must be preserved.

## Request Interpretation

Create `crates/w9pt-fs-state-postgres`, a runtime-specific PostgreSQL adapter that:

- implements the finalized `FilesystemStateStore` trait;
- advertises `WriterTopology::SerializableMultiWriter`;
- accepts a caller-owned SQLx `PgPool`;
- maps the complete semantic record model to normalized PostgreSQL tables;
- uses primary-only serializable reads and commits;
- preserves mutation replay, content publication, leases, fences, and revision polling;
- runs the shared state-store conformance suite against PostgreSQL 15–18.

This change does not store per-block mappings in PostgreSQL. The current state-store proposal publishes a `ContentRef`; changing content-index placement requires a separate proposal that first modifies the semantic state/storage contract.

## Dependency and Driver Selection

The adapter depends on `w9pt-fs-state` and SQLx, not on `w9pt`.

Use an exact SQLx `=0.8.6` dependency with default features disabled and PostgreSQL/Tokio support. SQLx 0.9 currently requires Rust 1.86, which exceeds the workspace Rust 1.85 baseline. Avoid compile-time `query!`/`query_as!` macros for ordinary SQL so consumers do not require `DATABASE_URL` or checked-query metadata at build time; use static parameterized SQL and explicit checked row decoding.

The adapter should not force a TLS trust policy. Callers constructing `PgPool` may enable a compatible SQLx TLS feature through Cargo feature unification. Pool creation, credentials, URLs, TLS roots, timeouts outside adapter transactions, and runtime lifecycle remain caller-owned.

Relevant verified references:

- SQLx 0.8.6 package: <https://docs.rs/crate/sqlx/0.8.6>
- SQLx runtime/TLS feature separation: <https://docs.rs/sqlx/latest/sqlx/index.html>
- SQLx MSRV change for 0.9: <https://github.com/launchbadge/sqlx/blob/main/CHANGELOG.md>

## Supported PostgreSQL Versions

Use PostgreSQL 15-compatible SQL and test current minor releases of PostgreSQL 15, 16, 17, and 18. PostgreSQL 14 is near its November 2026 end-of-support date and is excluded from the initial support matrix. PostgreSQL 19 is beta as of this proposal and is also excluded.

Reference: <https://www.postgresql.org/support/versioning/>

## Construction and Configuration

```rust
pub struct PostgresStateStore {
    pool: sqlx::PgPool,
    config: PostgresStateConfig,
    contract: StateStoreContract,
}

impl PostgresStateStore {
    pub async fn open(
        pool: sqlx::PgPool,
        config: PostgresStateConfig,
    ) -> Result<Self, PostgresStateError>;

    pub async fn migrate(
        pool: &sqlx::PgPool,
    ) -> Result<MigrationReport, PostgresStateError>;
}
```

`open` never runs DDL. It validates the fixed schema version/checksums, server version, primary status, required privileges, limits, and durability settings. `migrate` is explicit so deployment can use a migration role different from the runtime role.

`PostgresStateConfig` contains only adapter behavior:

- state limits from the finalized trait crate;
- statement and lock timeouts;
- bounded retry count for definitive serialization/deadlock aborts;
- bounded recovery attempts for ambiguous commits;
- primary-WAL durability policy.

Every bound is checked at construction.

## Schema Direction

Use a fixed quoted schema named `w9pt_fs_state_v1`. All runtime SQL and migration SQL fully qualifies every object; no correctness depends on `search_path` or caller-provided identifiers.

Prefer normalized permanent logged tables:

- filesystem/revision head;
- inodes and their `ContentRef` columns;
- directory entries;
- opens, open pins, and orphans;
- locks;
- xattrs and xattr staging;
- mutation results;
- writer fence/active lease state;
- idempotent lease-operation results;
- whole-commit change-event headers and ordered changed keys;
- embedded migration version/checksum records.

Avoid cascade deletes that create unreported state changes. Every semantic deletion is explicit in the transaction plan and change event.

## Lossless PostgreSQL Mapping

- Fixed-width IDs and digests use `BYTEA` with exact `octet_length` checks.
- Entry/xattr names and other byte-preserving ordered values use bounded `BYTEA`, avoiding locale-sensitive text comparison.
- Object keys use bounded `TEXT` because `w9pt-storage::ObjectKey` is checked UTF-8.
- Enums use small numeric tags plus named check constraints.
- Every public `u64` that can exceed `i64::MAX` uses constrained `NUMERIC(20,0)`.
- The adapter binds canonical decimal text with an explicit numeric cast and selects numeric values as text for checked Rust `u64` parsing.
- Timestamps and lease deadlines use `TIMESTAMPTZ` where the finalized state contract permits database time; caller-supplied filesystem timestamps retain their exact seconds/nanoseconds fields when PostgreSQL timestamp precision would lose information.

No unchecked `as i64` conversion is permitted. Rust preflight and SQL constraints independently enforce the same limits and relationships.

## Read Transactions

Every `ReadBatch` uses one connection and one `SERIALIZABLE READ ONLY` transaction:

1. Begin the transaction and set isolation before the first data query.
2. Verify `pg_is_in_recovery() = false` inside the transaction.
3. Read the filesystem revision.
4. Execute all typed point/range queries in request order.
5. Verify an `AtLeast` revision floor when requested.
6. Commit the read transaction before returning one `StateSnapshot`.

PostgreSQL `READ COMMITTED` is insufficient because each statement may observe a new snapshot. PostgreSQL documents that serializable transactions either correspond to a serial execution or fail with a serialization error: <https://www.postgresql.org/docs/18/sql-set-transaction.html>.

All ordered reads use keyset pagination rather than `OFFSET`:

- directory entries by `(cookie, name)`;
- locks by `(range_start, lock_id)`;
- xattrs by name;
- opens, orphans, leases, and mutations by stable primary-key suffix.

Results are bounded before materialization according to the state contract.

## Commit Transactions

Each commit uses one short `SERIALIZABLE READ WRITE` transaction:

1. Validate the complete request and aggregate bounds before SQL.
2. Set transaction-local statement/lock timeouts and `synchronous_commit = 'on'`.
3. Verify primary status.
4. Read the mutation ledger before current fence validation.
5. Return exact `AlreadyCommitted` for matching mutation ID, fingerprint, and client incarnation.
6. Return a hard mismatch for a retained mutation with different identity.
7. Lock the per-filesystem revision row.
8. Lock the writer-fence row and validate lease ID, holder, token, and database-time expiry.
9. Sort and deduplicate affected semantic `RecordKey`s.
10. Lock existing records in canonical order and perform absent-key predicate reads.
11. Validate every typed precondition and cross-record invariant.
12. Apply all normalized changes.
13. Allocate one checked filesystem revision.
14. Insert the exact mutation result, one whole-commit event, and ordered changed keys.
15. Commit once.

The per-filesystem revision row creates a short total-order serialization point for change polling. Different filesystems do not share that row. Its contention must be benchmarked before claiming high write throughput.

## SQLSTATE and Retry Semantics

Classify errors by SQLSTATE and known constraint name, never localized error text.

- `40001`: serialization abort known not committed; bounded exact retry is safe.
- `40P01`: deadlock abort known not committed; bounded exact retry is safe and should be observable as a performance/invariant signal.
- `23505`: only known named constraints map to semantic conflicts or ledger races.
- `23503`/`23514`/`22003`/`22P02`: model, range, or schema invariant failure.
- `25006`: read-only or incorrect primary routing.
- `57014`: timeout/cancellation classified by operation phase.
- connection and shutdown classes: availability or potentially ambiguous commit depending on phase.

Known-abort retry reruns the exact SQL transaction and never changes mutation identity or semantically rebases the request. Retry exhaustion is explicit.

An error from `COMMIT` is considered potentially committed unless PostgreSQL definitively reports transaction abort. Recovery uses a fresh primary transaction and the exact original mutation request. Ledger-first replay resolves an already committed attempt; an absent record permits only the same request to be tried again. Exhaustion returns an explicit ambiguous status without claiming rollback.

## Leases and Fencing

PostgreSQL time is the authoritative lease time source. Acquire, renew, and release use serializable transactions and stable lease-operation IDs.

The `writer_fences` row permanently retains the greatest token allocated for each `(filesystem, writer scope)`. Expiry or release clears active lease fields but never deletes/reset the counter. Takeover increments the token with checked numeric arithmetic. Every non-replayed commit validates the exact lease ID, holder incarnation, token, and non-expiration using database time in the same transaction.

Lease transitions participate in per-filesystem revision ordering and change polling. `LISTEN/NOTIFY` is not authoritative and is not required.

## Durability Boundary

Successful SQL `COMMIT` is the adapter acknowledgment boundary. Before advertising primary-WAL durability, `open` verifies:

- `pg_is_in_recovery() = false`;
- `fsync = on`;
- `full_page_writes = on`;
- permanent logged state tables;
- transaction-local `synchronous_commit = on` can be enforced.

This initial adapter promises WAL flush on the primary. It does not advertise synchronous-standby failover durability. A later change may add a separately verified policy requiring configured synchronous standbys and a stronger `synchronous_commit` mode. PostgreSQL documents these distinctions at <https://www.postgresql.org/docs/current/warm-standby.html>.

The adapter cannot prove storage hardware, backup, or failover quality beyond the exact verified database boundary.

## Change Polling

`poll_changes` uses a primary-only serializable read transaction:

1. Read the oldest retained and current filesystem revisions.
2. Return `RevisionCompacted` when the cursor is too old.
3. Select a bounded number of event headers after the cursor using revision keyset ordering.
4. Fetch changed keys ordered by `(revision, ordinal)`.
5. Enforce event/key/byte bounds before producing results.
6. Return a resume revision and `has_more`.

One transaction event is never split. Notifications may later wake pollers but can never substitute for revision reads.

## Testing and CI

Offline tests cover codecs, limits, key ordering, configuration, SQLSTATE classification, and migration checksums without a database.

Live tests use environment-provided PostgreSQL instances and two independently created pools/store clients. Required CI variables:

```text
W9PT_POSTGRES_15_DSN
W9PT_POSTGRES_16_DSN
W9PT_POSTGRES_17_DSN
W9PT_POSTGRES_18_DSN
```

`W9PT_POSTGRES_TEST_DSN` may select one local test database. When `W9PT_POSTGRES_TEST_REQUIRED=1`, missing DSNs fail rather than skip. Database/container provisioning remains outside the repository.

Live tests cover migrations, primary/durability validation, the shared conformance suite, serializable races, ledger replay/mismatch, prepared-content publication, lease/fence races, change gaps, commit-response loss, and empty-client recovery.

## Affected Files

- `Cargo.toml`, `Cargo.lock`, and `.gitattributes`.
- `crates/w9pt-fs-state-postgres/Cargo.toml`.
- Embedded migration SQL and modules for config, errors, numeric/key/row conversion, migrations, validation, transactions, reads, commits, leases, changes, and store implementation.
- Offline tests and environment-driven PostgreSQL integration tests.
- `README.md` and `.dev/project.md`.

No changes should be required in `w9pt`, `w9pt-storage`, their persistent formats, or the future content index.

## Risks

- The prerequisite state trait may change before implementation.
- Per-filesystem revision locking may limit a hot filesystem's write throughput.
- Serializable retries can amplify load under contention.
- PostgreSQL numeric, collation, or timestamp conversions can silently narrow public semantics if not independently tested.
- Commit-time disconnects are inherently ambiguous.
- Proxies can route an apparently valid connection to a standby.
- Migrations and runtime binaries can disagree during rolling deployment.
- Named constraints and SQLSTATE mappings can drift across migrations.
- Mutation/change retention may grow without a safe maintenance horizon.
- PostgreSQL storage or failover settings may provide weaker durability than the deployment advertises.
