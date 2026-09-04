# w9pt-fs-state

`w9pt-fs-state` defines the runtime-neutral authoritative metadata and coordination
contract for a local or distributed `w9pt` filesystem. It owns semantic records,
one-revision reads, serializable commits, mutation replay, writer fencing, and
revision invalidation. Immutable file bytes and manifests remain in
`w9pt-storage`; 9P wire/session state remains outside this crate.

The database is the sole publisher of an inode's current `ContentRef`. A content
write first prepares immutable objects through `w9pt-storage`, then submits
`PublishContent` in the metadata transaction. The state adapter never reads or
writes object storage.

## Adapter obligations

Every writable production adapter must construct a validated
`StateStoreContract::production`, implement `testing::StateStoreConformanceHarness`,
and await `testing::check_state_store_conformance` against independently opened
clients sharing one fresh authority. The harness supplies deterministic lease-time
advancement, change-log compaction, and commit-failure injection using
adapter-appropriate test controls. Passing the suite is necessary but does not
replace deployment-specific durability and failover evidence.

| Adapter | Expected topology | Required implementation evidence |
|---|---|---|
| SQLite/local file | `SingleFencedWriter` | One filesystem-wide writer route; WAL/locking configuration; transactionally persisted records, mutation results, revisions, change events, lease token history, migrations, and an `fsync` policy matching advertised acknowledgment. File loss or host loss is outside the guarantee unless the deployment proves otherwise. |
| PostgreSQL | `SerializableMultiWriter` | Serializable native transactions with bounded retries; indexed semantic scans; atomic mutation ledger and change log; database-authoritative leases/time; monotonically increasing token allocation across failover; durable commit settings; schema versioning and migrations. |
| etcd | Deployment-validated | Linearizable reads and compare/transaction mappings that preserve the complete multi-record transition; key/value and transaction size bounds proven for the configured workload; durable mutation/lease/token records; revision compaction mapped to explicit gaps. Do not store bulk content in etcd. |
| SlateDB/object-backed LSM | `SingleFencedWriter` | One fenced writer route; durable manifest/WAL boundary; recovery, compaction, and object-GC policy; atomic representation of state/result/event publication; persistent token history; bounded scans and schema migration. Object-store consistency alone is not sufficient. |

An adapter must not downgrade the public semantics to the weakest operation its
database exposes. If it cannot prove linearizable authoritative reads,
one-revision batches, serializable atomic commits, durable acknowledgment, atomic
result publication, stable revisions, monotonic fencing, and configured bounds,
production construction must fail.

## Commit and recovery rules

The mandatory order is exposed as `COMMIT_PROTOCOL_ORDER`:

1. Look up the mutation ledger and replay an exact retained result before checking
   a possibly expired fence.
2. Validate all request bounds and shapes, the exact current fence, and every
   typed precondition.
3. Stage the complete transition and validate cross-record invariants.
4. Allocate one revision and atomically publish records, the terminal mutation
   result, and one whole-commit change event.
5. Acknowledge only at the adapter's advertised durable boundary.

If an adapter loses certainty after publication, it returns `CommitOutcome::Ambiguous`.
The caller resolves it only by retrying the exact mutation ID, fingerprint, client
incarnation, and operands. A different request under the same mutation ID is a
hard mismatch.

## Memory reference

`testing::MemoryAuthority` is a deterministic semantic reference with shared,
cache-free clients, manual lease time, bounded history, failure injection, and
ordered traces. Its contract is explicitly `DeterministicReference`; it does not
claim restart or production durability.
