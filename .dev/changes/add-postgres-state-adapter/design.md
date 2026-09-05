# Design: PostgreSQL Filesystem State Adapter

## 1. Boundary and Finalized Contract

`w9pt-fs-state-postgres` is a runtime-specific implementation of the finalized `FilesystemStateStore` contract:

```text
FilesystemStateStore semantic requests
  -> PostgreSQL adapter validation and mapping
       -> caller-owned SQLx PgPool
            -> writable PostgreSQL primary
```

`add-filesystem-state-store` is approved and complete. The public Rust types and helpers in `crates/w9pt-fs-state` are normative. This adapter owns SQL mapping, migrations, transaction discipline, primary/durability validation, SQLSTATE classification, and PostgreSQL conformance. It does not own credentials, pool/runtime lifecycle, filesystem policy, content bytes, block maps, 9P sessions, or deployment.

## 2. SQLx and Caller-Owned Pool

Pin SQLx exactly to `0.8.6`, disable default features, and enable PostgreSQL plus Tokio runtime support. SQLx 0.9 requires Rust 1.86 while this workspace remains on Rust 1.85.

The adapter accepts an existing `PgPool`. It does not read connection URLs, credentials, TLS roots, pool sizes, or environment variables. Applications select a compatible SQLx TLS feature through Cargo feature unification.

Use static parameterized runtime SQL with explicit row decoding rather than compile-time query macros, so consumer builds require neither a live `DATABASE_URL` nor checked-query metadata.

```rust
pub struct PostgresStateStore {
    pool: sqlx::PgPool,
    config: PostgresStateConfig,
    contract: StateStoreContract,
    // Production construction always selects PostgreSQL wall time.
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

`open` validates but never migrates. `migrate` is explicit so migration and runtime roles can differ.

## 3. Fixed Schemas and Migrations

Production state uses the fixed schema `w9pt_fs_state_v1`. Every production SQL identifier is static and fully qualified; correctness never depends on `search_path`, locale, or caller-supplied identifiers.

Migration SQL is embedded with `include_str!`, assigned monotonic versions, normalized to LF, and hashed. `migrate`:

1. Acquires one fixed PostgreSQL advisory migration lock.
2. Creates the fixed schema and migration ledger when absent.
3. Reads all applied versions and checksums.
4. Rejects checksum drift, gaps, or unknown newer versions.
5. Applies each pending migration transactionally.
6. Records its version and checksum in the same transaction.
7. Releases the advisory lock.

`open` fails unless the exact supported migration set is present. Advisory locking is used only for migrations and never as a filesystem writer fence.

The feature-gated conformance harness may create the separate fixed `w9pt_fs_state_test_v1` schema and its manual-clock table. That schema is test support, is not part of production migrations, and is never selected by `PostgresStateStore::open`.

## 4. Private Authority Head and Empty Bootstrap

The finalized conformance sequence acquires a writer lease in an empty authority before inserting the public `FilesystemRecord`. Therefore private revision state and the public filesystem record must not share presence semantics.

`authority_heads` is a private table keyed by `filesystem_id` containing:

- current authoritative revision;
- oldest retained change cursor.

Absence of an authority-head row is interpreted as an empty authority at `StateRevision(1)` with oldest cursor `1`. A first successful revision-bearing operation lazily inserts the baseline row, locks it, and advances it. Concurrent creation uses `INSERT ... ON CONFLICT DO NOTHING` inside the serializable retry protocol.

`filesystem_records` separately stores the public `StateRecord::Filesystem` fields:

- filesystem ID;
- state revision;
- record revision;
- root inode ID;
- next directory cookie;
- policy generation.

Consequences:

- acquiring or renewing a lease does not make `ReadQuery::Filesystem` return a record;
- bootstrap may still require `RecordAbsent(RecordKey::Filesystem(...))`;
- public filesystem deletion does not erase the private revision/change/fence history;
- reads and polls against a never-seen filesystem return revision-one empty-authority semantics.

## 5. Relational Model

The production schema contains permanent logged tables:

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

Key mapping follows finalized `RecordKey` identities exactly:

- `filesystem_records`: `(filesystem_id)`.
- `inodes`: `(filesystem_id, inode_id)`.
- `directory_entries`: `(filesystem_id, parent_inode_id, name)`; unique `(filesystem_id, parent_inode_id, cookie)`.
- `opens`: `(filesystem_id, open_id)` with an inode index.
- `open_pins`: `(filesystem_id, inode_id, open_id)`.
- `orphans`: `(filesystem_id, inode_id)`.
- `locks`: `(filesystem_id, inode_id, lock_id)` with range-conflict indexes.
- `xattrs`: `(filesystem_id, inode_id, name)`.
- `xattr_staging`: `(filesystem_id, staging_id)` with an inode index.
- `mutation_results`: `(filesystem_id, mutation_id)`.
- `writer_fences`: `(filesystem_id, writer_scope_id)`.
- `writer_lease_operations`: `(filesystem_id, lease_operation_id)`.
- `change_commits`: `(filesystem_id, revision)` with an origin tag and origin ID.
- `change_keys`: `(filesystem_id, revision, ordinal)` with a normalized semantic-key encoding.

`writer_fences` retains the greatest-ever fencing token even when its active lease fields are null. Only a row with active fields decodes as a public `WriterLeaseRecord`; release makes the public point read absent while preserving private token history.

Avoid `ON DELETE CASCADE`. Every public record change and every emitted change key is explicit. Named constraints duplicate stable structural bounds and relationships; targeted Rust/SQL validation remains responsible for the complete semantic transition.

## 6. Lossless Type Mapping

| State value | PostgreSQL representation |
|---|---|
| Fixed 16-byte ID | `BYTEA` plus exact-width check |
| 32-byte digest/fingerprint | `BYTEA` plus exact-width check |
| Entry/xattr/principal/group/symlink bytes | bounded `BYTEA` |
| Object key | bounded `TEXT` with control-character and nonempty checks |
| Enum | numeric tag plus named check |
| Public `u64` | `NUMERIC(20,0)` constrained to `0..18446744073709551615` |
| Nonzero counter | constrained `NUMERIC(20,0)` with lower bound `1` |
| Filesystem timestamp | signed seconds plus nanoseconds fields |
| Lease deadline/duration | unsigned integer microsecond ticks in `NUMERIC(20,0)` |

Unsigned values are bound as canonical decimal text with an explicit `numeric` cast. Results are selected in canonical decimal form and parsed with checked Rust conversion. No unchecked signed cast or floating-point numeric conversion is permitted.

`ContentRef` is flattened into file ID, generation, logical size, manifest key, manifest digest, and storage-method tag, then reconstructed with `ContentRef::from_persisted`. Optional content fields are either all null or all present. The enclosing inode fields must agree with the reconstructed content reference.

## 7. Lease Clock Representation

Version 1 defines one `LeaseDuration` tick as one microsecond. Production transactions obtain authoritative time from PostgreSQL:

1. Evaluate `clock_timestamp()` exactly once for the operation.
2. Convert its Unix-epoch value to an integral microsecond `NUMERIC(20,0)` without a floating-point round trip.
3. Validate the result is in the public `u64` domain.
4. Use that captured tick for every expiry comparison and deadline calculation in the transaction.

Deadlines are stored as integer ticks, not `TIMESTAMPTZ`, so every persisted `LeaseDeadline` round-trips exactly. Addition of the bounded requested duration is checked before mutation.

The conformance harness uses a test-only database row containing the current integer tick. Independent pools query the same row inside their transactions, and `advance_time` updates it with checked SQL arithmetic. This provides deterministic time without shared process RAM. Separate live tests exercise production `clock_timestamp()` conversion and expiry behavior.

## 8. Read Transaction Protocol

Each `ReadBatch` runs in one primary-only `SERIALIZABLE READ ONLY` transaction. Isolation is established before the first data query. The transaction verifies `pg_is_in_recovery() = false` and that the connection is not defaulted to a write-disabled route; the transaction itself is intentionally read-only. It then reads the private current revision or revision-one baseline, executes queries in request order, and commits before returning.

The adapter implements every finalized point query and scan:

| Scan | SQL order and exclusive cursor |
|---|---|
| Inodes | `inode_id` |
| Directory entries | `cookie` within one parent; cookie is unique per parent |
| Opens | `open_id` |
| Open pins | `(inode_id, open_id)` |
| Orphans | `inode_id` |
| Locks | `(inode_id, lock_id)` |
| Xattrs | `(inode_id, name BYTEA)` |
| Xattr staging | `staging_id` |
| Mutations | `mutation_id` |
| Writer leases | `writer_scope_id`, active rows only |

No semantic query uses `OFFSET`. Rows are streamed/decoded only until the item or byte bound is reached. If the first complete record cannot fit, return `ReadOutcome::ScanBoundTooSmall`; do not allocate the oversized page.

Return only finalized outcomes:

- `ReadOutcome::Snapshot` with one revision and positional results;
- `ReadOutcome::RevisionUnavailable` for an unsatisfied `AtLeast` floor;
- `ReadOutcome::MalformedRequest` when the request exceeds adapter limits;
- `ReadOutcome::ScanBoundTooSmall` when one complete record cannot fit.

## 9. Ledger-First Commit Protocol

The finalized order requires replay before adapter-limit and fence validation. The adapter uses a two-stage ledger-first path:

1. Perform a short primary `SERIALIZABLE READ ONLY` ledger probe using only filesystem ID and mutation ID.
2. If present, classify it exclusively with `MutationContext::classify_record` and return the exact retained result or finalized mismatch.
3. If absent, run `CommitRequest::validate_preflight` against the adapter's limits outside the write transaction.
4. Start one short `SERIALIZABLE READ WRITE` transaction and repeat the ledger lookup before any current-state validation.

Inside the write transaction after the second absent result:

1. Set local statement timeout, lock timeout, and `synchronous_commit = 'on'`.
2. Verify writable-primary status.
3. Lazily create and lock the private authority head.
4. Lock the writer-fence row and capture one authoritative clock tick.
5. Validate the exact scope, lease ID, holder, fencing token, and non-expiration.
6. Sort/deduplicate affected `RecordKey`s using Rust's finalized semantic ordering.
7. Lock existing records in that order and perform serializable predicate reads for absent keys.
8. Evaluate every typed precondition in request order.
9. Apply every typed change with checked arithmetic and targeted cross-record validation.
10. Allocate one checked state/record revision.
11. Insert the exact mutation result and one whole change event including its mutation key.
12. Commit once.

No precondition failure creates a mutation-result row. Every changed public record receives the new record revision; a changed public `FilesystemRecord` also receives the new state revision, matching `StateRecord::with_revision` semantics.

The adapter validates only the affected semantic closure and uses targeted indexed queries; it never loads an unbounded filesystem merely to call a whole-map validator. Namespace generation, cookie allocation, inode/content relationships, open pins/orphans, xattr staging, and deterministic lock conflicts must match the reference authority.

## 10. Replay Semantics

The mutation ledger stores the fields required by finalized `MutationRecord`: mutation ID, request fingerprint, client incarnation, writer scope/incarnation/token, exact terminal result, committed revision, retention, and record revision.

Replay classification has exactly four mismatch dimensions exposed by `MutationMismatch`:

- mutation ID;
- request fingerprint;
- client incarnation;
- retention.

The request fingerprint is contractually the fingerprint of the complete semantic request. The adapter does not add independent change-set or submitted-result comparisons and always returns the recorded result for an exact classification. Ambiguous-commit recovery resubmits the same owned `CommitRequest`, but resolution still uses the finalized classifier.

## 11. Deterministic Locking and Conflicts

Rust `RecordKey::Ord` is mirrored by a fixed family tag followed by canonical byte-ordered key components. Existing rows are locked by issuing table-specific `SELECT ... FOR UPDATE` statements in that semantic order after the private head and writer-fence rows.

Absent keys receive serializable predicate reads, and named uniqueness constraints close insert races. Lock-conflict detection is inode-scoped, uses checked range predicates, and selects the conflicting `LockId` in canonical order so `CommitConflictKind::LockConflict` matches the reference authority.

Duplicate change targets are rejected by finalized preflight. A uniqueness race involving the mutation ledger rolls back and starts a fresh ledger-first classification; raw SQL constraint errors never escape as filesystem outcomes.

## 12. Prepared Content and Xattr Staging

`PublishContent` persists only the validated `ContentRef` and inode summaries. It calls the finalized validation logic for mutation ID, preparation base, authoritative base, file ID, logical size, data generation, inode generation, and times. PostgreSQL performs no target-object I/O.

`PublishXattrStaging` atomically validates and removes the exact complete staging record and inserts/replaces the published xattr. Generic staging deletion plus xattr insertion cannot bypass this dedicated transition.

## 13. SQLSTATE, Retry, and Ambiguity

Classify by SQLSTATE, operation phase, and known constraint name, never localized messages:

```text
40001  serialization failure: definitive abort, bounded identical retry
40P01  deadlock: definitive abort, bounded identical retry and observability
23505  recognized uniqueness race: semantic conflict or fresh ledger resolution
23503  recognized referential invariant failure
23514  recognized check/invariant failure
22003  numeric range failure
22P02  invalid numeric/input form
25006  read-only routing failure
57014  timeout/cancellation classified by phase
08xxx  connection failure; ambiguous only if COMMIT may have executed
57P0x  shutdown/availability failure classified by phase
```

Unknown constraints/codes are adapter failures. Known-abort retry never changes the request, mutation context, preconditions, content preparation, or result.

Any error returned while executing `COMMIT` is potentially committed unless PostgreSQL definitively reports abort. Recovery starts with a fresh primary serializable ledger probe and resubmits only the same request. Exhaustion returns `CommitOutcome::Ambiguous(AmbiguousCommit)`; it never claims rollback.

## 14. Lease Operations and Fencing

Acquire, renew, and release use short serializable transactions and stable lease-operation IDs. Each operation performs its `operation_fingerprint` replay lookup before current lease, duration, or fence validation.

- A successful grant/takeover increments the permanent greatest token and publishes an active `WriterLeaseRecord`.
- Renewal validates the exact unexpired fence, never shortens the deadline, and preserves the token.
- Release validates the exact unexpired fence, clears active fields, and preserves the greatest token.
- Successful grant, renewal, and release each allocate a revision and emit one `ChangeOrigin::Lease` event for the writer-lease key.
- Rejected operations retain only the exact finalized replay outcome required by the lease-operation ledger and do not emit a public revision event.
- Operation-history exhaustion returns `LeaseRejection::OperationHistoryFull` without unbounded growth.
- Every non-replayed commit validates the active lease using the captured authoritative database tick.

Lease outcome rows persist the operation kind, finalized request fingerprint, and sufficient tagged outcome fields to reconstruct exact `AlreadyApplied` or rejection behavior.

## 15. Change Polling

`poll_changes` runs in one primary-only serializable read transaction:

1. Read the private oldest/current revisions or the revision-one baseline.
2. Return `RevisionUnavailable` for a future cursor.
3. Return `RevisionCompacted` for a cursor older than retained history.
4. Read event headers after the cursor in revision order.
5. Before loading keys, return `PollBoundTooSmall` if the first event exceeds `max_keys`.
6. Fetch complete keys in `(revision, ordinal)` order until the event/key bounds would be exceeded.
7. Return `Changes(ChangeBatch)` with `events`, `next`, and `current_revision`.

One event is never split. There is no adapter-specific `has_more`; callers infer remaining work by comparing `ChangeBatch::next()` with `current_revision()`. `LISTEN/NOTIFY` may later be a wake-up hint but is never authoritative.

## 16. Durability and Startup Validation

The initial adapter advertises the complete production state contract with primary-WAL durability only. `open` verifies:

- server major version 15–18;
- `pg_is_in_recovery() = false`;
- writable transaction state;
- exact migration versions/checksums;
- configured limits fit fixed schema maxima;
- runtime privileges cover required DML, row locks, and transaction settings;
- `fsync = on` and `full_page_writes = on`;
- every production state table is permanent/logged;
- transaction-local `synchronous_commit = 'on'` can be enforced.

Primary routing is checked inside every authoritative transaction because pools/proxies may route separate connections differently. Write transactions additionally verify their transaction is writable; read transactions remain intentionally `READ ONLY`.

Successful SQL `COMMIT` is the acknowledgment boundary. The adapter does not advertise synchronous-standby failover durability and cannot prove storage hardware, backup, or failover quality.

## 17. Testing

Offline tests cover numeric boundaries, fixed-width and bounded codecs, every record variant, semantic key ordering, scan cursor mapping, configuration, migration checksums, and SQLSTATE/constraint classification.

Live tests use explicit external DSNs and independent pools. They cover:

- migration idempotency/drift and startup validation;
- empty revision-one reads and lease-before-filesystem bootstrap;
- the complete reusable conformance suite using the database-resident test clock;
- all point queries and all ten scan families;
- namespace, content, open-unlinked, lock, xattr, and staging transitions;
- serializable races, exact replay/mismatch, and known-abort exhaustion;
- before-publication failure and post-commit lost acknowledgment;
- production database-time conversion, expiry, takeover, and stale fences;
- every `ReadOutcome`, `CommitOutcome`, lease outcome, and `ChangePollOutcome` variant;
- reconstruction from a fresh pool/store with no correctness-bearing local state.

CI runs the matrix against current PostgreSQL 15–18 minors. Local tests may skip live work when no DSN exists; required CI mode fails if its declared DSNs are absent. No testcontainer or database provisioning dependency is introduced.
