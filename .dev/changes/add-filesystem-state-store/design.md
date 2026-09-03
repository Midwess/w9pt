# Design: Filesystem State Store Contract

## 1. Boundary and Ownership

`w9pt-fs-state` owns the portable authoritative state model and the contract a storage adapter must satisfy. It does not execute 9P operations, decide authorization, prepare file payloads, or expose database mechanics.

```text
w9pt Session
  -> future filesystem semantic engine
       ├── w9pt-storage ContentRepository
       │     -> immutable target objects
       └── w9pt-fs-state FilesystemStateStore
             -> authoritative metadata adapter
```

The future engine owns operation meaning and order:

1. Resolve session handles and principal/export context.
2. Read a consistent state snapshot.
3. Evaluate authorization and semantic preconditions.
4. Prepare immutable content when needed.
5. Submit one bounded declarative state commit.
6. On conflict, reread and revalidate the complete operation.
7. Return the exact committed or replayed result to the session layer.

## 2. Decision: Semantic Transactions, Not Generic KV

### Context

Create, rename, link/unlink, content publication, open-unlinked lifetime, and locks require atomic changes across multiple logical records. A public `get`/`put`/`delete` trait would force the engine or each adapter to recreate transaction semantics differently and could permit partial state changes.

### Options

1. Generic KV plus compare-and-swap: simple adapters, insufficient namespace and invariant semantics.
2. Database-style transaction callback: expressive, but holds adapter transactions across arbitrary caller logic and is difficult to express runtime-neutrally.
3. Consistent read plus declarative commit plan: short adapter transaction, explicit retries, bounded inputs, and testable semantics.

### Decision

Use consistent `ReadBatch` snapshots followed by a typed `CommitRequest`. Adapters validate all preconditions and changes and then apply the plan atomically. No adapter transaction remains open while immutable content is uploaded.

## 3. Dependency Direction

```text
w9pt-fs-state -> w9pt-storage
w9pt          -> no new dependencies

future-w9pt-filesystem -> w9pt + w9pt-fs-state + w9pt-storage
```

The state crate reuses portable content values but does not depend on protocol/session values. Database adapters therefore do not depend on 9P codecs, tags, fids, effects, or errno mappings.

## 4. Store Contract and Writer Topology

`StateStoreContract` is an enforceable declaration, not an optimization hint. A writable production adapter must prove:

- linearizable authoritative reads;
- one-revision consistent read batches;
- serializable multi-record commits;
- durable commit acknowledgment;
- atomic state, result-ledger, and change-event publication;
- stable revisions and record versions;
- monotonic fencing across expiry, release, restart, and takeover;
- bounded scans, commits, leases, and change retention.

Writer topology is explicit:

```rust
pub enum WriterTopology {
    SerializableMultiWriter,
    SingleFencedWriter,
}
```

`SerializableMultiWriter` permits independent clients to submit transactions and returns typed conflicts when serialization fails. `SingleFencedWriter` requires a filesystem-wide current writer and rejects every stale writer. Neither topology weakens atomicity, durability, idempotency, or fencing.

Expected future mappings:

- SQLite: `SingleFencedWriter` for one authoritative server.
- PostgreSQL: `SerializableMultiWriter` with database-backed lease and fence records.
- etcd: only after its bounded transaction and retained-revision workload passes conformance.
- SlateDB: `SingleFencedWriter`, with mutation routing, object-store CAS, compaction, and GC owned by the adapter/runtime.

## 5. IDs, Revisions, and Portable Handles

All persistent identities are fixed-width, caller-supplied or store-allocated under checked rules, and never encode a pointer or process-local collection index.

Core identities include:

```text
FilesystemId
InodeId
OpenId
LockId
LeaseId
WriterScopeId
WriterIncarnationId
ClientIncarnationId
```

Monotonic values include:

```text
StateRevision
RecordRevision
DirectoryCookie
FencingToken
inode/data/directory generations
```

Zero is reserved where a value needs an explicit absent/uninitialized representation. Checked increment methods return typed overflow errors and never wrap.

`InodeId` is not implicitly converted into `w9pt_storage::FileId`. A regular inode stores an explicit immutable content-file identity. Every prepared-content publication verifies this binding.

## 6. Authoritative Records

### Filesystem and inode

`FilesystemRecord` identifies the stable root and allocation/generation state. `InodeRecord` stores kind, permissions, owner/group, caller-supplied timestamps, logical size, link count, generations, and kind-specific data.

Regular files store an explicit content-file identity and optional `ContentRef`. The following invariant is mandatory whenever content exists:

```text
inode.content_file_id == ContentRef.file_id
inode.size == ContentRef.logical_size
inode.data_generation == ContentRef.generation
```

Directories store a directory generation. Symlink and special-node values are bounded and kind-checked.

### Namespace and cookies

`DirectoryEntryRecord` is identified semantically by `(filesystem, parent inode, entry name)` and stores the child inode and a stable directory cookie. Cookies are never derived from vector offsets, hashes, or target listing order and are not reused within a directory generation domain.

### Opens and orphans

`OpenRecord` contains a portable open ID, inode, client/session incarnation, access state, and retained inode generation. Open pins that affect inode lifetime are authoritative records.

Unlink updates the directory entry, inode link count, orphan state, and timestamps atomically. The final open release removes the last pin and permits orphan retirement in one transaction.

### Locks and xattrs

`LockRecord` uses checked byte-range endpoints or an explicit through-EOF representation, a portable owner/open identity, lock kind, and generation. Conflict selection is deterministic.

Xattr metadata and staging state are bounded. Large staged bytes may later use immutable content references; bulk xattr bytes are not silently placed in metadata records.

## 7. Consistent Read Protocol

`ReadBatch` contains a filesystem ID, consistency requirement, and bounded query vector. Every result is read at one authoritative `StateRevision` and remains positionally matched to its query.

```rust
pub enum ReadConsistency {
    LatestLinearizable,
    AtLeast(StateRevision),
}
```

`AtLeast` requires a revision freshness floor but does not require historical snapshots. An adapter may return a newer revision.

Point and range queries are typed. Every range query includes maximum items and bytes plus a stable resume cursor. A target/database adapter must reject excessive results before materializing beyond the bound.

## 8. Commit Preconditions and Changes

Preconditions describe observations whose stability protects the mutation:

- record absent or present at an exact revision;
- inode and data generation equality;
- directory generation equality;
- exact base content identity;
- link/open-pin count equality;
- exact active writer fence.

State changes are typed insert/replace/delete operations for one record family plus checked counter/generation updates. A record key and value variant must match exactly.

All preconditions and changes are validated before the adapter mutates state. The memory reference stages a complete cloned delta before swapping it into the authoritative map; production adapters use one native serializable transaction.

## 9. Prepared Content Publication

Content publication is a dedicated `PublishContent` change. The request carries `PreparedContent`, the target inode, and its expected base.

Before commit, the store verifies:

- commit and preparation mutation IDs agree;
- preparation base equals the authoritative inode content;
- content file ID equals the inode's explicit content-file binding;
- new logical size and data generation agree with `ContentRef`;
- every immutable dependency was prepared before the state commit;
- inode size, timestamps, content root, and data generation are changed together.

The store does not read object storage. `PreparedContent` is the proof/handoff from `w9pt-storage`; adapter-specific publication stores its portable fields atomically with inode metadata.

## 10. Mutation Ledger and Ambiguity

`MutationContext` includes:

```text
filesystem ID
mutation ID
complete request fingerprint
client incarnation
writer scope/epoch
retention horizon
```

`MutationResult` contains a bounded opaque result with an explicit semantic kind and codec version. This avoids coupling the state crate to `w9pt` while permitting the future engine to recover its exact terminal semantic result.

The mutation lookup occurs before fence validation:

- matching retained identity returns `AlreadyCommitted` and the exact result;
- mismatching fingerprint or client incarnation returns a hard mismatch;
- absent mutation proceeds to fence and precondition validation.

This ordering lets a client recover an already committed result after its lease expires. A database error that leaves commit status unknown is resolved only by retrying the exact mutation identity; it is never converted into a blind new mutation.

Ledger deletion is out of scope. Records carry retention horizons so a future maintenance protocol can remove them only after a caller-proven safe replay boundary.

## 11. Lease and Fence Protocol

Lease operations are themselves idempotent through stable operation IDs. Duration and retained operation counts are bounded.

The adapter owns deadline evaluation using an explicit clock/time authority supplied at construction. The state crate does not call the system clock. Distributed adapters may use an authoritative database time source through their caller-owned implementation.

Safety rules:

1. Each new grant allocates a token greater than every token previously granted for the scope.
2. Renewal preserves the token and may extend the deadline.
3. Release invalidates the lease without resetting the token counter.
4. An unexpired lease cannot be stolen.
5. Every non-replayed commit validates lease ID, holder incarnation, scope, token, and expiry.
6. Expiry permits takeover but never authorizes the expired token again.

## 12. Revisions and Cache Invalidation

Each successful commit allocates one `StateRevision`. Every changed record receives a corresponding `RecordRevision`. One `ChangeEvent` lists the bounded semantic record keys changed by that complete transaction.

`poll_changes` returns whole events after a requested revision. It never splits one commit. If the requested point predates retained history, it returns `RevisionCompacted { oldest_available }` so the consumer discards its cache and performs authoritative reads.

Change polling is an invalidation optimization. Correctness never depends on a cache or uninterrupted notifications.

## 13. Limits and Errors

`StateLimits` bounds at least:

- names, principals/groups, symlinks, and xattr metadata;
- terminal mutation results;
- queries per read batch and returned scan items/bytes;
- preconditions and changes per commit;
- aggregate transaction bytes;
- locks, open pins, and xattrs touched by one transaction;
- lease duration and retained lease operation results;
- change events, polling batches, and retained history.

Errors distinguish invalid configuration, malformed records, range/arithmetic failures, resource limits, corruption, unsupported guarantees, and adapter failures. Semantic results distinguish conflicts, mutation mismatch, stale fence, expired lease, and compacted revision.

## 14. Deterministic Memory Authority

The reference implementation uses one shared backing authority and cloneable independently opened clients. Clients contain no correctness-bearing cache.

It provides:

- one locked authoritative revision for each read batch;
- complete staged commits with atomic publication;
- durable-in-model mutation replay;
- manual caller-controlled time;
- monotonically increasing fences;
- bounded change history;
- ordered operation tracing;
- failure injection before and after the atomic commit point.

An after-commit injected failure is ambiguous. Retrying the same mutation returns its recorded result. Reopening a new client sees the complete old or complete new state, never a partial transaction.

The memory implementation is a semantic oracle and does not by itself prove real durable-storage behavior.

## 15. Adapter Conformance

Reusable conformance covers:

- contract validation;
- one-revision batch reads and stable scans;
- atomic create, rename, link/unlink, setattr, and prepared-content publication;
- independent client conflicts and retries;
- exact mutation replay and mismatch rejection;
- lease acquire/renew/release, expiry, takeover, and stale fences;
- open-unlinked pins and orphan retirement;
- overlapping/disjoint locks and xattr state;
- bounded change polling and compaction gaps;
- failure before/after commit and empty-local-state reopen;
- every configured size/count/arithmetic limit.

Every production adapter must add schema/migration tests and failure injection around its real durability boundary in addition to this shared semantic suite.
