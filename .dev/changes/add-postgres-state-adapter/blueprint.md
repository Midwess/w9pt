# Implementation Blueprint: Add PostgreSQL State Adapter

## Dependency Gate

This change depends on `add-filesystem-state-store` and must remain draft until `w9pt-fs-state` is approved, implemented, and its conformance API is stable. Before implementation, reconcile every adapter task against the finalized public trait and schema model.

## Design Approach

Create `w9pt-fs-state-postgres`, a Tokio/SQLx adapter for `FilesystemStateStore`:

```text
future filesystem engine
  -> w9pt-fs-state::FilesystemStateStore
       -> w9pt-fs-state-postgres
            -> caller-owned sqlx::PgPool
                 -> PostgreSQL primary
```

The adapter accepts a caller-created pool, advertises `SerializableMultiWriter`, uses primary-only serializable transactions, maps semantic records to a fixed normalized schema, and preserves exact replay/fencing/revision behavior. It stores no file payload, S3 manifest, per-block mapping, or 9P session state.

## Package and Public API

Use SQLx exactly `0.8.6` because the current SQLx 0.9 release requires Rust 1.86 while this workspace targets Rust 1.85:

```toml
sqlx = {
    version = "=0.8.6",
    default-features = false,
    features = ["postgres", "runtime-tokio"]
}
```

Use static parameterized runtime queries and checked explicit row conversion. Embedded migration SQL uses `include_str!` and a small adapter-owned migration runner/checksum ledger, avoiding a live `DATABASE_URL` during consumer builds.

Do not force a TLS backend. The embedding application constructs `PgPool` and may enable its chosen compatible SQLx TLS feature through Cargo feature unification.

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

    pub fn pool(&self) -> &sqlx::PgPool;
}
```

`open` validates and never migrates. `migrate` is explicit so schema and runtime roles may have different privileges.

## Fixed Schema and Migrations

Use one fixed quoted schema:

```sql
"w9pt_fs_state_v1"
```

All runtime and migration statements fully qualify identifiers. No correctness depends on `search_path`, locale, or caller-generated identifiers.

Migrations are immutable SQL files embedded with `include_str!`, assigned monotonic versions, and checked with a stable digest. A fixed PostgreSQL advisory lock serializes migration runners. Applied version/digest records live in `w9pt_fs_state_v1.schema_migrations`. Startup fails on unknown versions, missing migrations, or checksum drift.

Migration execution is transactional wherever PostgreSQL permits. The initial migration creates the schema, tables, constraints, and indexes. Future rolling changes must use documented expand/contract compatibility.

## Physical Schema

Create permanent logged tables:

```text
schema_migrations
filesystems
inodes
directory_entries
opens
orphans
open_pins
locks
xattrs
xattr_staging
mutation_results
writer_fences
writer_lease_operations
change_commits
change_keys
```

Principal keys and indexes:

- `filesystems`: primary key `filesystem_id`; root inode, current revision, oldest retained change revision.
- `inodes`: primary key `(filesystem_id, inode_id)` and record revision.
- `directory_entries`: primary key `(filesystem_id, parent_inode_id, name)`; unique `(filesystem_id, parent_inode_id, cookie)`; child-inode index.
- `opens`: primary key `(filesystem_id, open_id)`; inode index.
- `orphans`: primary key `(filesystem_id, inode_id)`.
- `open_pins`: primary key `(filesystem_id, inode_id, open_id)`.
- `locks`: primary key `(filesystem_id, lock_id)`; index `(filesystem_id, inode_id, range_start, lock_id)`.
- `xattrs`: primary key `(filesystem_id, inode_id, name)`.
- `xattr_staging`: primary key `(filesystem_id, staging_id)`; owner/deadline indexes.
- `mutation_results`: primary key `(filesystem_id, mutation_id)`; fingerprint, client incarnation, writer identity, exact result, revision, and retention horizon.
- `writer_fences`: primary key `(filesystem_id, writer_scope_id)`; greatest-ever fence plus nullable current lease/holder/deadline.
- `writer_lease_operations`: primary key `(filesystem_id, lease_operation_id)` for acquire/renew/release replay.
- `change_commits`: primary key `(filesystem_id, revision)` and unique mutation ID.
- `change_keys`: primary key `(filesystem_id, revision, ordinal)` with typed normalized key components.

Avoid cascading deletes. Every record mutation and change key is explicit.

Use deferrable foreign keys only where ordering would otherwise prevent a valid atomic rename, unlink, or orphan transition. Rust validation remains the primary semantic validator; named SQL constraints independently reject malformed direct rows.

## Lossless SQL Mapping

- Fixed IDs and digests: `BYTEA` with exact `octet_length` checks.
- Entry names, xattr names, and byte-preserving identities: bounded `BYTEA` with binary ordering.
- Object keys: bounded `TEXT`.
- Enum values: small numeric tags with named check constraints.
- Full public unsigned values: `NUMERIC(20,0)` plus `0..18446744073709551615` checks.
- Filesystem timestamps: exact seconds/nanoseconds columns when PostgreSQL timestamp precision would narrow the public model.
- Lease deadlines: `TIMESTAMPTZ`, evaluated using PostgreSQL time.

The numeric codec binds canonical unsigned decimal text through an explicit numeric cast and reads `numeric::text`, then parses through checked Rust `u64`. No unchecked signed cast is used.

## Startup Validation

`PostgresStateStore::open` verifies:

- PostgreSQL major version 15–18;
- `pg_is_in_recovery() = false`;
- exact supported schema migration versions and checksums;
- configured state limits fit the schema constraints;
- runtime role can perform required reads, writes, transaction settings, and row locks;
- `fsync = on` and `full_page_writes = on`;
- permanent state tables are logged;
- transaction-local `synchronous_commit = on` can be enforced.

The initial contract promises WAL flush on the primary only. Synchronous-standby durability and replica reads are separate future capabilities.

Because a pool or proxy may route different connections differently, primary status is also verified inside every authoritative transaction.

## Serializable Read Mapping

Every `ReadBatch`:

1. Acquires one pool connection.
2. Begins a transaction and immediately selects `SERIALIZABLE READ ONLY`.
3. Verifies the connection is a primary.
4. Reads the current filesystem revision.
5. Executes all point/range queries in request order.
6. Checks an `AtLeast` freshness floor inside the same snapshot.
7. Commits before returning one state snapshot.

Range scans use stable keyset cursors, never `OFFSET`:

- directory entries `(cookie, name)`;
- locks `(range_start, lock_id)`;
- xattrs `name`;
- opens/orphans/mutations stable key suffixes.

Each query and aggregate result is bounded before materialization.

## Serializable Commit Mapping

Each attempt runs in one short `SERIALIZABLE READ WRITE` transaction:

1. Validate the complete model and bounds before SQL.
2. Set transaction-local statement timeout, lock timeout, and `synchronous_commit = on`.
3. Verify primary status.
4. Read the mutation ledger first.
5. Return exact `AlreadyCommitted` for matching mutation ID, fingerprint, and client incarnation.
6. Reject a retained identity mismatch before current-fence checks.
7. Lock the per-filesystem revision row.
8. Lock and validate the exact database-time writer fence.
9. Sort and deduplicate all affected `RecordKey`s.
10. Lock existing records in canonical order and read absent-key predicates.
11. Validate typed preconditions and cross-record invariants.
12. Apply normalized record changes.
13. Increment the filesystem revision using checked numeric arithmetic.
14. Store one exact mutation result, whole-commit event header, and ordered changed keys.
15. Commit once.

All records changed by one semantic transaction receive the same new record revision. Failed preconditions roll back without retaining a terminal mutation result.

The per-filesystem revision row intentionally creates total event order and a short serialization point. Different filesystems do not share it.

## Content Publication

`PublishContent` writes only checked `ContentRef` fields into the inode row. The adapter delegates semantic validation to the finalized state model and confirms prepared mutation, file identity, base content, size, and generation relationships before issuing SQL.

No PostgreSQL table contains file payload bytes, object manifests, or per-block/per-extent mappings. Adding database-resident content mappings first requires a separate approved change to the state/storage contract.

## SQLSTATE, Retry, and Ambiguity

Use exact SQLSTATE values and named constraints, never localized messages:

- `40001`: definitive serialization abort; bounded exact transaction retry.
- `40P01`: definitive deadlock abort; bounded exact retry plus observability.
- `23505`: only known constraints map to semantic conflict or mutation-ledger race.
- `23503`, `23514`, `22003`, `22P02`: invalid model/schema/range outcome.
- `25006`: primary/read-only routing failure.
- `57014`: timeout/cancellation classified by transaction phase.
- connection/shutdown errors: availability or ambiguous commit according to phase.

Definitive retry repeats the exact request and never rebases or changes mutation identity. Retry exhaustion is explicit.

Any error returned while executing `COMMIT` is treated as potentially committed unless PostgreSQL proves abort. Recovery opens a fresh primary transaction and resubmits the identical request. Ledger-first replay returns the committed result if present; if absent, only the same request may proceed. Bounded recovery exhaustion returns explicit ambiguity without claiming rollback.

## Database-Time Leases and Fencing

Lease operations use stable operation IDs and serializable primary transactions. PostgreSQL time evaluates deadlines.

- Acquire locks the persistent scope row.
- A non-expired different holder receives a busy result.
- Fresh acquisition increments the greatest-ever fence using checked numeric arithmetic.
- Renew validates exact lease, holder, and token and preserves the token.
- Release clears active fields but retains the fence counter.
- Exact lease-operation outcomes are retained for ambiguous replay.
- Every non-replayed state commit validates the current non-expired fence.
- Lease transitions receive filesystem revisions and change events.

No Rust process clock participates in lease correctness.

## Change Polling

`poll_changes` runs in a primary-only serializable read transaction:

1. Read oldest retained/current revisions.
2. Return `RevisionCompacted` for an old cursor.
3. Select bounded event headers after the cursor in revision order.
4. Select corresponding keys ordered by `(revision, ordinal)`.
5. Enforce commit/key/byte bounds.
6. Return resume revision and `has_more`.

One event is never split. `LISTEN/NOTIFY` may later be used only as a wake-up hint.

## Files to Create or Modify

```text
Cargo.toml
Cargo.lock
.gitattributes
README.md
.dev/project.md

crates/w9pt-fs-state-postgres/
  Cargo.toml
  migrations/
    0001_initial.sql
  src/
    lib.rs
    config.rs
    error.rs
    numeric.rs
    key_codec.rs
    row_codec.rs
    migration.rs
    validation.rs
    transaction.rs
    read.rs
    commit.rs
    lease.rs
    change.rs
    store.rs
    sql/
      mod.rs
      records.rs
      locks.rs
  tests/
    support/
      mod.rs
    numeric_roundtrip.rs
    migration.rs
    validation.rs
    postgres_conformance.rs
    transaction_isolation.rs
    idempotency.rs
    commit_recovery.rs
    leases_and_fencing.rs
    change_polling.rs
```

## Implementation Phases

1. Reconcile finalized trait, scaffold adapter, configure SQLx, and implement checked codecs/configuration.
2. Create fixed schema and explicit embedded migration runner.
3. Implement startup/transaction validation and SQLSTATE classification.
4. Implement consistent reads and bounded keyset scans.
5. Implement ledger-first atomic commits, deterministic locks, revisions, and prepared-content publication.
6. Implement database-time leases, fencing, and change polling.
7. Run PostgreSQL 15–18 conformance, ambiguity/failure tests, documentation, and dependency validation.

## Integration Test Environment

CI supplies explicit DSNs:

```text
W9PT_POSTGRES_15_DSN
W9PT_POSTGRES_16_DSN
W9PT_POSTGRES_17_DSN
W9PT_POSTGRES_18_DSN
```

`W9PT_POSTGRES_TEST_DSN` selects one optional local database. When `W9PT_POSTGRES_TEST_REQUIRED=1`, missing required DSNs fail rather than skip. Tests use unique random filesystem identities inside the fixed schema and delete only those scoped rows. Database/container provisioning remains outside this repository.

## Testing Strategy

- Round-trip `0`, `i64::MAX`, `i64::MAX + 1`, and `u64::MAX` through every unsigned mapping.
- Reject malformed direct rows through named SQL constraints.
- Prove one read batch cannot mix revisions during concurrent commits.
- Prove per-filesystem ordering and cross-filesystem independence.
- Exercise namespace, content, open-unlinked, lock, and xattr transitions through the shared conformance suite.
- Retry exact mutations through separate pools and reject changed fingerprints/results.
- Lose the response after server commit and recover only through ledger replay.
- Reopen fresh stores with empty process-local state.
- Exercise lease acquire/renew/release/takeover and stale writers with database time.
- Verify standby and weak-durability configurations are rejected.
- Exercise change polling at empty, bounded, resume, and compaction boundaries.
- Run the complete adapter conformance matrix on PostgreSQL 15–18 current minors.

## Risks and Mitigations

- **Prerequisite trait changes:** block implementation and reconcile the full schema after the state crate lands.
- **Hot revision row:** keep it per filesystem and transactions short; benchmark before claiming throughput.
- **Serializable retry amplification:** deterministic locks, bounded exact retries, and explicit exhaustion.
- **Absent-key races:** combine serializable predicate reads with named unique constraints.
- **Ambiguous commit:** never infer rollback or alter the request; resolve through exact ledger retry.
- **Unsigned narrowing:** constrained numeric values and independent boundary tests.
- **Replica staleness:** primary verification inside every transaction.
- **Search-path/collation behavior:** fixed fully qualified schema and byte-preserving ordered values.
- **Migration drift:** embedded immutable checksums and fail-closed startup.
- **Durability overstatement:** verify exact settings and document primary-WAL-only acknowledgment.
