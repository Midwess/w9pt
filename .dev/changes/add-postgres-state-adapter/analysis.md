# Analysis: Add PostgreSQL State Adapter

## Current State

- The workspace contains `w9pt`, `w9pt-fs-storage`, and `w9pt-fs-state`.
- `add-filesystem-state-store` is approved and complete at `37/37`; commit `6efc340` introduced the finalized contract and reusable conformance suite.
- Existing workspace tests, formatting, and Clippy with warnings denied pass after the state-store implementation.
- `w9pt` remains a dependency-free Sans-I/O protocol/session core.
- `w9pt-fs-storage` prepares immutable content and portable `ContentRef`/`PreparedContent` values but does not publish authoritative inode state.
- `w9pt-fs-state` owns the runtime-neutral authoritative records, typed reads, declarative commits, leases/fences, semantic outcomes, and conformance harness.
- The PostgreSQL change was originally drafted before the state contract landed. This revision reconciles its schema and protocols against the final Rust API and clears the implementation gate.

## Finalized Contract Inventory

The adapter must implement these exact entry points:

```text
contract
read(ReadBatch) -> ReadOutcome
commit(CommitRequest) -> CommitOutcome
acquire_writer_lease(AcquireWriterLease) -> AcquireLeaseOutcome
renew_writer_lease(RenewWriterLease) -> RenewLeaseOutcome
release_writer_lease(ReleaseWriterLease) -> ReleaseLeaseOutcome
poll_changes(ChangePoll) -> ChangePollOutcome
```

Finalized public record families are filesystem, inode, directory entry, open, open pin, orphan, lock, xattr, xattr staging, mutation, and active writer lease.

Finalized scans are:

```text
inodes                 inode_id
directory entries      cookie within one parent
opens                  open_id
open pins              (inode_id, open_id)
orphans                inode_id
locks                  (inode_id, lock_id)
xattrs                 (inode_id, name bytes)
xattr staging          staging_id
mutations              mutation_id
writer leases          writer_scope_id
```

The adapter must return the public outcome enums exactly. It must not add an adapter-only scan cursor, replay mismatch, `has_more` field, or semantic error variant.

## Resolved Pre-Implementation Gaps

### Dependency gate

The prior statement that `w9pt-fs-state` was draft and absent is obsolete. The dependency is complete and the PostgreSQL proposal is now approved.

### Empty bootstrap and revisions

The conformance suite acquires and renews a lease before inserting `FilesystemRecord`. The earlier schema conflated the public filesystem row with the revision head, which could not represent that sequence.

Resolution: introduce private `authority_heads` and separate public `filesystem_records`. An absent private head means an empty authority at revision one. A successful pre-bootstrap lease transition creates/advances the private head while the public point query remains absent.

### Scan identities

The earlier draft used `(cookie, name)` for directory scans, `(range_start, lock_id)` for lock scans, and `(filesystem_id, lock_id)` as a lock primary key.

Resolution: use the finalized cursor/key forms exactly—directory cookie, `(inode, lock)` lock cursor, and `(filesystem, inode, lock)` record identity. Also implement the finalized inode, open-pin, and xattr-staging scans that the old task list omitted.

### Lease time representation

The earlier draft stored `LeaseDeadline` as `TIMESTAMPTZ`, but the public contract uses exact unsigned ticks and PostgreSQL timestamps have microsecond precision and a different numeric domain.

Resolution: define one version-1 tick as one microsecond, store deadlines/durations as constrained `NUMERIC(20,0)`, and convert one captured PostgreSQL `clock_timestamp()` observation to an integer Unix-epoch tick. The conformance harness uses a separate test-only database integer clock so `advance_time` is deterministic across independent pools.

### Replay semantics

The earlier text proposed independent change-set/result mismatch checks. The finalized `MutationContext::classify_record` exposes only mutation ID, fingerprint, client incarnation, and retention mismatch dimensions.

Resolution: delegate replay classification to the finalized helper. The fingerprint is contractually the complete semantic request identity; the adapter never invents an additional mismatch variant and returns the stored result on exact replay.

### Change polling

The earlier draft returned `has_more`, which is not part of `ChangeBatch`, and did not enumerate every finalized outcome.

Resolution: return only `Changes`, `RevisionUnavailable`, `RevisionCompacted`, `PollBoundTooSmall`, or `MalformedRequest`. `Changes` contains events, `next`, and `current_revision`; remaining work is inferred by comparing the two revisions.

### Commit phase order

The earlier plan performed full adapter preflight before SQL, conflicting with the normative ledger-before-preflight rule for retained retries.

Resolution: perform a short primary `SERIALIZABLE READ ONLY` fixed-size ledger probe first. Only an absent mutation proceeds to adapter-limit preflight. Every write attempt repeats ledger lookup before fence/current-state validation inside its serializable transaction.

## Dependency and Driver Selection

The adapter depends on `w9pt-fs-state` and SQLx, not on `w9pt`.

Use exact SQLx `=0.8.6`, default features disabled, with PostgreSQL and Tokio runtime support. Avoid compile-time query macros so consumers need no build-time database. Callers constructing `PgPool` choose credentials, URLs, TLS, pool limits, and runtime lifecycle.

SQLx 0.9 currently raises MSRV to Rust 1.86, above this workspace's Rust 1.85 baseline. Reference: <https://github.com/launchbadge/sqlx/blob/main/CHANGELOG.md>.

## Supported PostgreSQL Versions

Use PostgreSQL 15-compatible SQL and test the current minor releases of PostgreSQL 15, 16, 17, and 18. PostgreSQL 19 remains prerelease as of this reconciliation and is excluded until a later compatibility change. Reference: <https://www.postgresql.org/support/versioning/>.

## Construction and Configuration

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

Production `open` always selects PostgreSQL wall time. Feature-gated test support may select a database-resident manual clock but cannot be selected implicitly by the production constructor.

`PostgresStateConfig` contains only adapter behavior:

- finalized `StateLimits`;
- statement and lock timeouts;
- bounded retry count for definitive serialization/deadlock aborts;
- bounded recovery attempts for ambiguous commits;
- primary-WAL durability policy.

Every bound is checked at construction and must fit immutable schema maxima.

## Schema Direction

Use fixed quoted production schema `w9pt_fs_state_v1`. All production runtime and migration statements fully qualify objects.

Private tables:

- `authority_heads`: per-filesystem current revision and oldest retained cursor;
- `writer_fences`: greatest token plus nullable active lease fields;
- `writer_lease_operations`: bounded exact acquire/renew/release replay rows;
- migration ledger and normalized change history.

Public record tables:

- `filesystem_records`, `inodes`, `directory_entries`;
- `opens`, `open_pins`, `orphans`;
- `locks`, `xattrs`, `xattr_staging`;
- `mutation_results`;
- active projections of `writer_fences` as writer-lease records.

Avoid cascading deletes. Fixed-width/numeric constraints and targeted cross-record validation independently protect the public model.

## Lossless PostgreSQL Mapping

- Fixed IDs and digests use exact-width `BYTEA`.
- Entry/xattr/principal/group/symlink values use bounded `BYTEA`.
- Object keys use bounded checked `TEXT`.
- Enums use stable numeric tags plus named checks.
- Every public `u64` uses `NUMERIC(20,0)` and canonical decimal text conversion.
- Filesystem timestamps retain signed seconds and nanoseconds separately.
- Lease deadlines use exact integer microsecond ticks, not timestamps.
- Optional `ContentRef` fields are all-null or all-present and reconstruct through the public storage constructor.

No unchecked `as i64` or floating-point numeric conversion is permitted.

## Read Semantics

Each `ReadBatch` uses one primary `SERIALIZABLE READ ONLY` transaction. It observes the private revision-one baseline when no authority head exists, preserves query order, implements every finalized point/scan form, and returns the exact finalized outcome.

Keyset queries stream bounded rows. If the next whole record exceeds the byte bound, return `ScanBoundTooSmall`; do not materialize it first.

## Commit Semantics

Commit processing is ledger-first:

1. Fixed-size primary serializable ledger probe.
2. Exact `MutationContext::classify_record` replay/mismatch when present.
3. Adapter-limit preflight only when absent.
4. Serializable write attempt with a repeated ledger-first lookup.
5. Private authority-head lock/create.
6. Writer-fence lock and one captured database tick.
7. Canonical semantic record locks and absent predicates.
8. Typed preconditions and targeted invariant validation.
9. Checked application of every finalized `StateChange`.
10. One revision, record versions, exact mutation record, and whole change event.
11. One durable `COMMIT`.

The adapter validates the affected semantic closure using bounded indexed queries. It does not load an unbounded filesystem to validate a transition.

## SQLSTATE, Retry, and Ambiguity

Classify failures by SQLSTATE, phase, and named constraint. Definitive `40001`/`40P01` aborts may retry the identical request within configured bounds. Unknown constraints are adapter failures.

A `COMMIT` response error is potentially committed unless PostgreSQL proves abort. Recovery begins with a fresh primary ledger probe and may resubmit only the same owned request. Exhaustion returns finalized `CommitOutcome::Ambiguous`.

## Lease and Change Semantics

Lease-operation ledger lookup precedes duration/current-fence validation. Successful acquire/renew/release transitions allocate revisions and emit one lease-origin event; rejected outcomes do not fabricate revision events. Release removes the public active lease while retaining the private greatest token.

Change polling returns whole events under event/key bounds. It explicitly handles future cursors, compacted history, and a first event that cannot fit. There is no `has_more` field; `next < current_revision` indicates another poll is needed.

## Testing and CI

Offline tests cover codecs, limits, cursor mappings, configuration, clock conversion, migration checksums, and SQLSTATE classification.

Live tests use explicit DSNs and two independent pools. A feature-gated harness creates one test-only database clock row so the shared suite can advance time without sleeps or shared RAM. Separate tests cover the production PostgreSQL clock path.

Required CI variables:

```text
W9PT_POSTGRES_15_DSN
W9PT_POSTGRES_16_DSN
W9PT_POSTGRES_17_DSN
W9PT_POSTGRES_18_DSN
```

`W9PT_POSTGRES_TEST_DSN` may select one local database. When `W9PT_POSTGRES_TEST_REQUIRED=1`, missing declared DSNs fail rather than skip.

## Readiness Conclusion

The prerequisite is complete, the design is reconciled, the proposal is approved, and implementation may begin at Task 1.2. Remaining work is implementation and verification, not unresolved contract design.
