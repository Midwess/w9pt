# Implementation Blueprint: Add Filesystem State Store

## Design Approach

Create `w9pt-fs-state`, a runtime-neutral authoritative filesystem metadata and coordination contract. It depends on `w9pt-storage` for immutable `ContentRef`/`PreparedContent` values and does not depend on `w9pt`.

```text
future filesystem semantic engine
  ├── w9pt-fs-state::FilesystemStateStore
  │     ├── deterministic memory authority
  │     ├── future SQLite/PostgreSQL adapters
  │     ├── future etcd adapter
  │     └── future SlateDB adapter
  └── w9pt-storage::ContentRepository
```

The trait models consistent reads, serializable conditional commits, leases and fencing, mutation-result idempotency, and bounded revision polling. It does not expose SQL, generic key/value transactions, SDK clients, runtimes, or object-store layout.

Use static dispatch with return-position `impl Future`, following `w9pt-storage::TargetStore`. Object-safe boxed adapters are deferred.

## Public Contract

```rust
pub trait FilesystemStateStore: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;

    fn contract(&self) -> StateStoreContract;

    fn read(
        &self,
        request: ReadBatch,
    ) -> impl Future<Output = Result<StateSnapshot, Self::Error>> + Send;

    fn commit(
        &self,
        request: CommitRequest,
    ) -> impl Future<Output = Result<CommitOutcome, Self::Error>> + Send;

    fn acquire_writer_lease(
        &self,
        request: AcquireWriterLease,
    ) -> impl Future<Output = Result<AcquireLeaseOutcome, Self::Error>> + Send;

    fn renew_writer_lease(
        &self,
        request: RenewWriterLease,
    ) -> impl Future<Output = Result<RenewLeaseOutcome, Self::Error>> + Send;

    fn release_writer_lease(
        &self,
        request: ReleaseWriterLease,
    ) -> impl Future<Output = Result<ReleaseLeaseOutcome, Self::Error>> + Send;

    fn poll_changes(
        &self,
        request: ChangePoll,
    ) -> impl Future<Output = Result<ChangePollOutcome, Self::Error>> + Send;
}
```

`StateStoreContract` describes proven semantics, including:

```rust
pub enum WriterTopology {
    SerializableMultiWriter,
    SingleFencedWriter,
}
```

Both topologies require linearizable authoritative reads, serializable atomic multi-record commits, durable commit acknowledgment, atomic mutation-result persistence, bounded operations, and monotonically increasing fencing. A single-writer adapter accepts only the filesystem-wide writer scope. A multi-writer adapter may support independently fenced scopes while serializing conflicting record updates.

## Strong Types and Records

Define:

- `FilesystemId`, `InodeId`, `OpenId`, `LockId`, `LeaseId`;
- `WriterScopeId`, `WriterIncarnationId`, `ClientIncarnationId`;
- `StateRevision`, `RecordRevision`, `FencingToken`, `DirectoryCookie`;
- `RequestFingerprint`, `MutationRetention`, checked timestamps and lease durations;
- bounded `EntryName`, `XattrName`, principal/group IDs, symlink targets, and terminal-result bytes.

Reuse `w9pt_storage::MutationId`, `ContentRef`, `PreparedContent`, and `BaseContentIdentity` where their semantics match. A distinct state request fingerprint covers the entire filesystem mutation rather than only content preparation.

Initial authoritative records:

- `FilesystemRecord`: root inode, allocation generations, and filesystem policy version;
- `InodeRecord`: kind, mode, owner/group, timestamps, size, link count, inode/data generations, explicit content-file binding, and optional `ContentRef`;
- `DirectoryEntryRecord`: parent, bounded name, stable cookie, and child;
- `OpenRecord`: portable open identity, inode, client/session incarnation, access state, and retained generation;
- `OrphanRecord`: unlinked inode plus durable open pins;
- `LockRecord`: inode, checked byte range, kind, owner, open identity, and lock generation;
- `XattrRecord` and `XattrStagingRecord`;
- `MutationRecord`: mutation ID, complete fingerprint, client incarnation, writer epoch, result, committed revision, and retention horizon;
- `WriterLeaseRecord`: scope, holder incarnation, lease identity, deadline, and fencing token.

All records have checked constructors and variant/key validation. No record contains a pointer, process-local index, SDK handle, cache address, or wire-only object.

## Consistent Reads

```rust
pub struct ReadBatch {
    pub filesystem_id: FilesystemId,
    pub consistency: ReadConsistency,
    pub queries: Vec<ReadQuery>,
}

pub enum ReadConsistency {
    LatestLinearizable,
    AtLeast(StateRevision),
}

pub struct StateSnapshot {
    pub revision: StateRevision,
    pub results: Vec<ReadResult>,
}
```

`ReadQuery` supports typed point reads and bounded ordered scans for directory entries, locks, xattrs, opens, orphans, leases, and mutation records. Every result in one batch comes from the same authoritative revision and remains positionally associated with its query.

`AtLeast` is a freshness floor rather than a request for historical MVCC. An adapter may return a newer linearizable revision. Ordered scans carry an explicit item/byte limit and a stable resume key or directory cookie.

## Declarative Commit

```rust
pub struct MutationContext {
    pub mutation_id: MutationId,
    pub fingerprint: RequestFingerprint,
    pub client_incarnation: ClientIncarnationId,
    pub retention: MutationRetention,
}

pub struct CommitRequest {
    pub filesystem_id: FilesystemId,
    pub mutation: MutationContext,
    pub fence: WriterFence,
    pub preconditions: Vec<Precondition>,
    pub changes: Vec<StateChange>,
    pub terminal_result: MutationResult,
}
```

Typed preconditions include:

- record absence or exact `RecordRevision`;
- inode and data generations;
- exact base `ContentRef`/`BaseContentIdentity`;
- directory generation;
- link count and open-pin count;
- active exact writer fence.

Typed state changes include insert/replace/delete operations for every record family and checked counter/generation updates. These are semantic record operations, not public adapter keys.

`PublishContent` is a special change rather than a generic inode replacement. It validates:

- prepared mutation ID equals the commit mutation ID;
- prepared file ID equals the inode's explicit content-file binding;
- prepared base identity equals the current inode content;
- inode logical size equals the prepared `ContentRef` size;
- generation movement is valid.

The store never fetches target data and never accepts an unprepared raw `ContentRef` for a content mutation.

Commit processing order is normative:

1. Look up `(filesystem ID, mutation ID)`.
2. When the retained fingerprint and client incarnation match, return the exact result without reapplying changes, even if the original fence has expired.
3. Reject the same mutation ID with a different fingerprint or identity.
4. Validate bounds, writer lease/fence, typed preconditions, and all record invariants.
5. Stage every state change without exposing partial state.
6. Atomically assign one revision, apply all records, append one change event, and retain the terminal result.
7. Durably acknowledge according to the adapter contract.

```rust
pub enum CommitOutcome {
    Committed(CommittedMutation),
    AlreadyCommitted(CommittedMutation),
    Conflict(CommitConflict),
    Rejected(CommitRejection),
}
```

A target/transport failure may leave commit status unknown. Safe recovery retries the identical mutation ID, fingerprint, and client incarnation. Conflicts are not ledger results and require the semantic engine to reread, reauthorize, and reprepare content where necessary.

## Leases and Fencing

Lease requests carry stable operation IDs so ambiguous acquire, renew, and release calls are replayable. Lease durations are bounded. The adapter evaluates deadlines with an explicit authoritative clock/time source supplied at construction; the core reads no process-global clock.

For each `(filesystem, writer scope)`:

- a fresh grant after release or expiry allocates a token greater than all prior tokens;
- renew preserves the token;
- release invalidates the grant but never resets the counter;
- an unexpired grant cannot be stolen;
- every commit validates scope, lease ID, holder incarnation, token, and non-expiration;
- stale and expired fences are rejected even if the old process continues running.

Leases coordinate liveness; fencing tokens provide safety.

## Change Polling

```rust
pub struct ChangePoll {
    pub filesystem_id: FilesystemId,
    pub after: StateRevision,
    pub max_commits: usize,
}

pub enum ChangePollOutcome {
    Batch(ChangeBatch),
    RevisionCompacted { oldest_available: StateRevision },
}
```

Polling is bounded and nonblocking rather than a runtime-specific stream. One event represents one whole transaction and contains its revision, mutation ID, and bounded changed-record keys. Polling never splits a commit. Consumers use events only for cache invalidation; authoritative reads still validate revisions.

## Files to Create or Modify

```text
Cargo.toml
Cargo.lock
README.md
.dev/project.md

crates/w9pt-fs-state/
  Cargo.toml
  src/
    lib.rs
    contract.rs
    ids.rs
    limits.rs
    time.rs
    records.rs
    read.rs
    commit.rs
    lease.rs
    change.rs
    error.rs
    validation.rs
    store.rs
    testing/
      mod.rs
      clock.rs
      memory_store.rs
      conformance.rs
  tests/
    domain_validation.rs
    read_consistency.rs
    commit_atomicity.rs
    idempotency.rs
    content_publication.rs
    leases_and_fencing.rs
    change_polling.rs
    crash_boundaries.rs
    state_store_conformance.rs
```

No shared database serialization format is introduced in this change. Each future adapter owns a versioned physical schema while losslessly representing the public semantic model and enforcing the common bounds. Opaque terminal results carry an explicit kind/version.

## Implementation Phases

### Phase 1: Crate and checked model

Add the crate, dependency boundary, strong IDs, bounded values, validated limits, record keys, and authoritative record types.

### Phase 2: Store and read contract

Define required guarantees, writer topology, typed queries/results, consistent batch semantics, and the runtime-neutral trait.

### Phase 3: Atomic commit and idempotency

Define mutation contexts, preconditions, changes, prepared-content publication, terminal results, outcomes, and invariant validation.

### Phase 4: Leases and change polling

Define idempotent lease operations, fencing behavior, time authority, revision events, compaction gaps, and polling bounds.

### Phase 5: Deterministic reference implementation

Implement one shared memory authority with independently opened clients, injected manual time, complete transaction staging, deterministic tracing, and before/after-commit failure injection.

### Phase 6: Conformance and documentation

Create a reusable asynchronous conformance suite, exercise two independent clients, document adapter obligations, and run all workspace validation gates.

## Testing Strategy

- Validate zero, maximum, overflow, mismatched record kind, invalid name, invalid range, and inconsistent inode/content cases.
- Read through two clients and prove a batch never mixes revisions.
- Inject failure around staged commit publication; observers see the complete old or complete new record set.
- Retry an ambiguous committed mutation and receive the exact recorded result without applying changes twice.
- Reuse a mutation ID with changed fingerprint, client incarnation, operands, or bytes and receive a hard mismatch.
- Publish content only after immutable preparation and reject wrong file, mutation, base, size, or generation.
- Exercise deterministic conflicting and disjoint transactions.
- Expire and reacquire leases; prove stale tokens fail and tokens never repeat after release or client recreation.
- Test open-unlinked survival, final-open removal, overlapping/disjoint locks, rename overwrite, link counts, xattr staging, and simultaneous attribute changes as atomic record sets.
- Poll across empty, multi-commit, bounded, resumed, and compacted revision histories.
- Reopen fresh clients with empty local state and prove authoritative behavior is unchanged.
- Require each future production adapter to run the reusable conformance suite.

## Risks and Mitigations

- **Generic changes admit invalid filesystem state:** use typed preconditions, special content publication, checked records, and cross-record validation.
- **Memory tests hide distributed failures:** use factory-based independent clients, ambiguous outcomes, deterministic schedules, and mandatory adapter conformance.
- **Lease clocks disagree:** adapters use an explicit time authority; safety relies on monotonic fencing rather than time alone.
- **Mutation ledgers grow indefinitely:** every result carries a retention horizon and cannot be removed before a caller-proven safe point.
- **Large scans or commits amplify resources:** validate aggregate counts and encoded lengths before allocation or execution.
- **RPITIT is not object-safe:** accept static dispatch in version 1 and defer a boxed facade.
- **Adapter schemas diverge:** require schema versions, lossless public-model round trips, migrations, and shared conformance.
- **Change-log compaction leaves caches stale:** return an explicit revision-gap result requiring authoritative reload.
