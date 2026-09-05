# Implementation Blueprint: Add PostgreSQL State Adapter

## Readiness

`add-filesystem-state-store` is approved and complete. This blueprint is reconciled with the finalized `w9pt-fs-state` API, and the PostgreSQL change is approved for implementation.

## Design Approach

Create `w9pt-fs-state-postgres`, a Tokio/SQLx implementation of `FilesystemStateStore`:

```text
future filesystem engine
  -> w9pt-fs-state::FilesystemStateStore
       -> w9pt-fs-state-postgres
            -> caller-owned sqlx::PgPool
                 -> PostgreSQL writable primary
```

The adapter advertises `SerializableMultiWriter`, uses primary-only serializable transactions, maps every finalized semantic record/cursor/outcome to a fixed normalized schema, and stores no content payload, target manifest object, per-block mapping, or 9P session state.

## Package and Public API

Use SQLx exactly `0.8.6`:

```toml
sqlx = {
    version = "=0.8.6",
    default-features = false,
    features = ["postgres", "runtime-tokio"]
}
```

Use static parameterized runtime queries and checked explicit row conversion. Do not require `DATABASE_URL` or SQLx query metadata during builds. The embedding application constructs `PgPool` and may add a compatible SQLx TLS feature through feature unification.

```rust
pub struct PostgresStateStore {
    pool: sqlx::PgPool,
    config: PostgresStateConfig,
    contract: StateStoreContract,
    clock: LeaseClockMode,
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

Production `open` always uses PostgreSQL wall time. Test-only construction may select the database manual clock required by the conformance harness.

## Fixed Production Schema

Use one fixed schema:

```sql
"w9pt_fs_state_v1"
```

Every production statement fully qualifies every object. Migrations are immutable embedded SQL with monotonic versions and stable checksums. A fixed advisory lock serializes migration runners. `open` never runs DDL.

Create permanent logged tables:

```text
schema_migrations
authority_heads
filesystem_records
inodes
directory_entries
opens
open_pins
orphans
locks
xattrs
xattr_staging
mutation_results
writer_fences
writer_lease_operations
change_commits
change_keys
```

## Empty Authority and Bootstrap

`authority_heads` is private and separate from `filesystem_records`.

- Missing head means current revision `1` and oldest cursor `1`.
- A first successful revision-bearing operation inserts a baseline head then increments it.
- Lease acquire/renew may occur while the public filesystem record is absent.
- `ReadQuery::Filesystem` reads only `filesystem_records`.
- A bootstrap commit may require public filesystem absence and insert the filesystem/root records.
- Public filesystem deletion never deletes the private head, mutation ledger, change history, or fence counter.

Use `INSERT ... ON CONFLICT DO NOTHING` plus a locked read under serializable retry for lazy head creation. Never expose a private head as `StateRecord::Filesystem`.

## Exact Physical Keys

- `authority_heads`: `(filesystem_id)`.
- `filesystem_records`: `(filesystem_id)` with state revision, record revision, root, next cookie, and policy generation.
- `inodes`: `(filesystem_id, inode_id)`.
- `directory_entries`: `(filesystem_id, parent_inode_id, name)` and unique `(filesystem_id, parent_inode_id, cookie)`.
- `opens`: `(filesystem_id, open_id)`.
- `open_pins`: `(filesystem_id, inode_id, open_id)`.
- `orphans`: `(filesystem_id, inode_id)`.
- `locks`: `(filesystem_id, inode_id, lock_id)`.
- `xattrs`: `(filesystem_id, inode_id, name)`.
- `xattr_staging`: `(filesystem_id, staging_id)`.
- `mutation_results`: `(filesystem_id, mutation_id)`.
- `writer_fences`: `(filesystem_id, writer_scope_id)`.
- `writer_lease_operations`: `(filesystem_id, lease_operation_id)`.
- `change_commits`: `(filesystem_id, revision)`.
- `change_keys`: `(filesystem_id, revision, ordinal)`.

`writer_fences` contains the permanent greatest token and nullable active holder/lease/deadline/revision fields. Active fields decode as `WriterLeaseRecord`; all-null active fields mean the public record is absent.

Avoid cascade deletes. Use deferrable foreign keys only where a valid atomic transition requires ordering flexibility, and still make every semantic change explicit.

## Lossless Row Codecs

- All state/storage IDs: exact 16-byte `BYTEA`.
- Digests and fingerprints: exact 32-byte `BYTEA`.
- Entry/xattr/principal/group/symlink bytes: bounded `BYTEA`.
- Object keys: nonempty bounded `TEXT` without forbidden controls.
- Enum variants: stable numeric tags with named constraints.
- Every `u64`: `NUMERIC(20,0)` constrained to the unsigned range.
- Every nonzero revision/generation/token: unsigned numeric plus lower bound one.
- `UnixTimestamp`: signed seconds plus nanoseconds.
- Lease duration/deadline: integer microsecond ticks in unsigned numeric columns.
- Optional `ContentRef`: all fields null or all fields present.

Bind unsigned values as canonical decimal text with explicit casts. Select numeric values canonically and parse with checked Rust conversion. Reconstruct `ContentRef` through `ContentRef::from_persisted` and validate its inode summaries.

## Production and Test Lease Clocks

Version 1 defines one lease tick as one microsecond.

Production clock query:

1. Call `clock_timestamp()` once per operation.
2. Convert the Unix epoch value to an integral microsecond `NUMERIC` without floating-point transport.
3. Reject negative/out-of-range values.
4. Reuse the captured value for all comparisons and deadline calculations.

Persist the resulting deadline as `NUMERIC(20,0)`, never `TIMESTAMPTZ`.

The feature-gated PostgreSQL test harness creates `w9pt_fs_state_test_v1.lease_clocks(clock_id, now_tick)` outside production migrations. Test clients query one row in the same transaction as lease/fence state. `advance_time` performs a checked database update. This keeps independent pools deterministic with no shared process clock.

## Startup Validation

`PostgresStateStore::open` verifies:

- PostgreSQL major version 15–18;
- `pg_is_in_recovery() = false`;
- writable transaction state;
- exact production schema migration versions/checksums;
- configured `StateLimits` fit schema maxima;
- runtime role has required DML, lock, and transaction-setting privileges;
- `fsync = on` and `full_page_writes = on`;
- every production state table is logged/permanent;
- transaction-local `synchronous_commit = on` can be enforced.

Primary routing is checked again inside every authoritative transaction. Write transactions additionally verify they are writable; read transactions remain intentionally `READ ONLY`.

## Serializable Read Mapping

Every `ReadBatch`:

1. Acquires one pool connection.
2. Starts `SERIALIZABLE READ ONLY` before the first data query.
3. Verifies writable-primary routing.
4. Reads the private authority revision or revision-one baseline.
5. Evaluates `AtLeast` in the same snapshot.
6. Executes queries in request order.
7. Streams bounded scan rows and constructs positional results.
8. Commits before returning one outcome.

Exact scan keysets:

```text
Inodes              inode_id
DirectoryEntries    cookie within parent
Opens               open_id
OpenPins            (inode_id, open_id)
Orphans             inode_id
Locks               (inode_id, lock_id)
Xattrs              (inode_id, name BYTEA)
XattrStaging        staging_id
Mutations           mutation_id
WriterLeases        writer_scope_id, active only
```

Never use `OFFSET`. If one complete next row cannot fit the byte bound, return `ScanBoundTooSmall`. Return only the finalized `ReadOutcome` variants.

## Ledger-First Commit Mapping

Before full adapter-limit preflight, perform a short primary `SERIALIZABLE READ ONLY` fixed-size lookup by `(filesystem_id, mutation_id)`:

- present: call `MutationContext::classify_record` and return exact replay/mismatch;
- absent: run `CommitRequest::validate_preflight` against adapter limits.

Each new write attempt then runs one short `SERIALIZABLE READ WRITE` transaction:

1. Set local statement timeout, lock timeout, and `synchronous_commit = on`.
2. Verify writable-primary status.
3. Repeat mutation-ledger lookup first.
4. Lazily create/lock the private authority head.
5. Lock the writer-fence row and capture one clock tick.
6. Validate the exact current unexpired fence.
7. Sort/deduplicate affected finalized `RecordKey`s.
8. Lock existing records in canonical semantic order.
9. Perform serializable absent-key predicate reads.
10. Evaluate typed preconditions in request order.
11. Apply typed changes and targeted cross-record validation.
12. Increment the authority revision with checked numeric arithmetic.
13. Assign the corresponding record revision to all changed public records.
14. Store the exact `MutationRecord`, including retention and writer context.
15. Store one whole change event including the mutation-record key.
16. Commit once.

Do not load an entire filesystem to validate a transition. Query the bounded affected closure and use constraints/indexed predicates for namespace generations, cookie allocation, content relationships, open pins/orphans, xattr staging, and lock conflicts.

## Exact Mutation Replay

Persist all finalized `MutationRecord` fields. Classification uses only the finalized mismatch dimensions:

```text
mutation ID
request fingerprint
client incarnation
retention
```

The request fingerprint represents the complete semantic request. Do not compare submitted changes or terminal results as separate adapter-specific mismatch dimensions. On exact replay, return the recorded revision/result and skip current fence validation.

## Semantic Lock Order

Mirror Rust `RecordKey::Ord` using a stable family tag plus byte-ordered key components. Lock the private authority head, then the writer-fence row, then issue table-specific `FOR UPDATE` operations in canonical semantic order.

For absent rows, perform predicate reads in the serializable transaction. For byte-range locks, search only the inode, apply checked overlap predicates, and choose the lowest canonical conflicting `LockId`.

## Content and Xattr Publication

`PublishContent` validates and persists only `ContentRef`, logical size, generations, and times. Target data and manifest objects are already durable and are never fetched by PostgreSQL.

`PublishXattrStaging` validates one complete staging record and atomically consumes it into the final xattr. Generic staging deletion plus generic xattr mutation must be rejected when it would bypass the dedicated transition.

## SQLSTATE, Retry, and Ambiguity

- `40001`: definitive serialization abort; bounded identical retry.
- `40P01`: definitive deadlock abort; bounded identical retry plus observability.
- `23505`: only known constraints map to a semantic race or fresh ledger resolution.
- `23503`/`23514`/`22003`/`22P02`: known model/range/schema failures.
- `25006`: read-only route.
- `57014`: timeout/cancellation classified by phase.
- `08xxx`/`57P0x`: availability or ambiguous commit according to phase.

Never match localized messages. Unknown constraints/codes remain adapter failures.

Any uncertain `COMMIT` response is potentially committed. Recovery starts with a fresh primary ledger probe and may resubmit only the same owned request. Exhaustion returns `CommitOutcome::Ambiguous`.

## Lease Mapping

For acquire, renew, and release:

1. Look up `(filesystem_id, lease_operation_id)` first.
2. Compare the finalized `operation_fingerprint` and reconstruct exact replay/rejection.
3. Validate request limits only when absent.
4. Start/repeat a short primary serializable transaction.
5. Lock the relevant private head/fence row in fixed order.
6. Capture one authoritative tick.
7. Apply the finalized grant/renew/release rule.
8. Retain bounded operation replay data.
9. For a successful public lease transition only, allocate a revision and whole lease-origin event.

Release nulls active fields but never deletes/resets the greatest token. Renewal never shortens a deadline. History exhaustion is explicit.

## Exact Change Polling

`poll_changes` uses one primary serializable read transaction:

1. Read oldest/current revisions or revision-one baseline.
2. Return `RevisionUnavailable` for a future cursor.
3. Return `RevisionCompacted` for a cursor before retained history.
4. Read headers after the cursor by revision.
5. Use stored key counts to return `PollBoundTooSmall` before fetching an oversized first event.
6. Fetch whole events and ordered keys within event/key bounds.
7. Return `Changes(ChangeBatch)` with events, `next`, and `current_revision`.

No event is split and no `has_more` field is added. Remaining work is `next < current_revision`.

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
    clock.rs
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
    testing.rs            # feature-gated database conformance controls
  tests/
    support/
      mod.rs
    numeric_roundtrip.rs
    migration.rs
    validation.rs
    bootstrap.rs
    postgres_conformance.rs
    transaction_isolation.rs
    scans.rs
    idempotency.rs
    commit_recovery.rs
    leases_and_fencing.rs
    change_polling.rs
```

## Implementation Phases

1. Scaffold the adapter, config/error types, SQLx dependency, and checked numeric/key/clock codecs.
2. Implement private authority heads, exact public record tables, and embedded migrations.
3. Implement startup/transaction validation and SQLSTATE classification.
4. Implement revision-one reads, every point query, and all ten exact scan mappings.
5. Implement ledger-first commits, semantic locking, all transitions, revisions, and exact replay.
6. Implement database-time leases, test database clock, fencing, and exact change outcomes.
7. Run PostgreSQL 15–18 conformance/failure tests and complete documentation/dependency validation.

## Integration Test Environment

CI supplies:

```text
W9PT_POSTGRES_15_DSN
W9PT_POSTGRES_16_DSN
W9PT_POSTGRES_17_DSN
W9PT_POSTGRES_18_DSN
```

`W9PT_POSTGRES_TEST_DSN` selects one optional local database. `W9PT_POSTGRES_TEST_REQUIRED=1` makes missing declared DSNs fail. Tests use unique filesystem, clock, and mutation identities and remove only their scoped rows. Database/container provisioning remains outside the repository.

## Required Verification

- Round-trip every public numeric boundary and malformed row.
- Prove empty revision-one behavior and lease-before-filesystem bootstrap.
- Prove one read batch cannot mix revisions.
- Test every point query, scan order, resume cursor, and byte-bound outcome.
- Prove per-filesystem ordering and cross-filesystem independence.
- Exercise every finalized state transition and semantic outcome.
- Retry exact mutations through separate pools and reject only finalized mismatch dimensions.
- Lose a successful commit acknowledgment and recover through ledger replay.
- Exercise deterministic test time and real PostgreSQL time separately.
- Prove release/expiry/takeover never reuses a fence.
- Exercise future, compacted, too-small, empty, bounded, and resumed change polls.
- Reopen fresh stores with no local state and pass the complete shared conformance suite on PostgreSQL 15–18.
