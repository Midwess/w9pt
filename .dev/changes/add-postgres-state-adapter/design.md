# Design: PostgreSQL Filesystem State Adapter

## 1. Boundary

`w9pt-fs-state-postgres` is a runtime-specific adapter for the finalized `FilesystemStateStore` contract:

```text
FilesystemStateStore semantic requests
  -> PostgreSQL adapter validation and mapping
       -> caller-owned SQLx PgPool
            -> writable PostgreSQL primary
```

The crate owns SQL mapping, migrations, transaction discipline, primary/durability validation, SQLSTATE classification, and PostgreSQL conformance. It does not own credentials, pool/runtime lifecycle, filesystem policy, content bytes, block maps, 9P sessions, or deployment.

## 2. Decision: SQLx 0.8.6 and Caller-Owned Pool

### Context

The workspace uses Rust 1.85. Current SQLx 0.9 requires Rust 1.86, while SQLx 0.8.6 supports the needed PostgreSQL transactions and pools within the workspace baseline.

### Decision

Pin SQLx exactly to `0.8.6`, disable default features, and enable only PostgreSQL and Tokio runtime support. Accept an already-created `PgPool`.

The adapter does not force a TLS feature or read environment variables. Applications select TLS features, certificates, URLs, credentials, pool limits, and executor lifecycle when constructing their pool.

Use static bound runtime SQL with explicit row decoding rather than compile-time query macros. This prevents consumer builds from requiring a live `DATABASE_URL` or checked-query metadata.

## 3. Decision: Fixed Fully Qualified Schema

Use the fixed schema `w9pt_fs_state_v1`. Every SQL identifier is a static fully qualified quoted name. No schema name comes from configuration and no runtime statement depends on `search_path`.

This prevents identifier injection, search-path redirection, and accidental sharing of similarly named tables. Multiple filesystems share the schema through their stable `filesystem_id` primary-key component.

## 4. Migration Protocol

Migration SQL is embedded with `include_str!`, versioned monotonically, normalized to canonical line endings, and hashed. A fixed PostgreSQL advisory lock serializes migration runners.

`PostgresStateStore::migrate`:

1. Acquires the fixed migration advisory lock.
2. Creates the fixed schema and migration ledger if absent.
3. Reads applied versions and checksums.
4. Rejects checksum drift, unknown newer versions, or gaps.
5. Applies each pending migration transactionally.
6. Records its version and checksum in the same transaction.
7. Releases the advisory lock.

`PostgresStateStore::open` never runs DDL and fails unless the exact supported schema is already present. Migration and runtime roles can therefore have different privileges.

Advisory locking is permitted only for migration serialization; it is not a filesystem writer fence.

## 5. Relational Model

### Filesystem revision head

`filesystems` contains the stable root inode, current authoritative revision, and oldest retained change revision. A commit locks this row to allocate one per-filesystem revision and total-order its change event.

### Inodes and namespace

`inodes` stores all portable inode fields and flattened optional `ContentRef` fields. `directory_entries` uses byte-exact names and unique constraints for both `(parent, name)` and `(parent, cookie)`.

No `ON DELETE CASCADE` hides state changes. Rename, overwrite, link/unlink, orphan creation, and inode retirement explicitly mutate every affected row.

### Opens, orphans, locks, and xattrs

Portable opens, open pins, orphans, locks, xattrs, and staging records use separate normalized tables and record revisions. Indexes support inode-scoped scans and stable keyset pagination.

### Mutation and lease ledgers

`mutation_results` is keyed by filesystem and mutation ID and stores the complete fingerprint/client identity, exact terminal result, commit revision, writer context, and retention horizon.

`writer_fences` permanently stores the greatest fence allocated for a scope plus optional active lease fields. `writer_lease_operations` retains exact acquire/renew/release results for ambiguity-safe replay.

### Change history

`change_commits` stores one header per committed revision. `change_keys` stores ordered normalized semantic record keys. The oldest retained revision is updated atomically with bounded history maintenance.

## 6. PostgreSQL Type Mapping

| State value | PostgreSQL representation |
|---|---|
| Fixed ID | `BYTEA` plus exact width check |
| Digest/fingerprint | fixed-width `BYTEA` |
| Entry/xattr name | bounded `BYTEA` with binary order |
| Object key | bounded `TEXT` |
| Enum | numeric tag plus named check constraint |
| Public `u64` | `NUMERIC(20,0)` with full-range check |
| Exact filesystem timestamp | seconds plus nanoseconds fields |
| Lease deadline | `TIMESTAMPTZ` evaluated by PostgreSQL |

Unsigned numeric parameters are sent as canonical decimal text and cast explicitly to `numeric`. Results are selected as canonical text and parsed with checked Rust conversion. This preserves `u64::MAX` without depending on signed `BIGINT`.

All byte and numeric bounds are validated before SQL and repeated as named database constraints.

## 7. Read Transaction Protocol

Each read batch uses one `SERIALIZABLE READ ONLY` transaction on a primary. Isolation is established before the first data query. The adapter reads the filesystem revision and every requested record/page from that snapshot, preserves result order, commits the transaction, and returns one `StateSnapshot`.

`AtLeast(R)` checks the revision inside the same transaction. A lower revision returns the finalized contract's freshness result rather than silently serving stale data.

Scans use keyset cursors and explicit limits. No query uses `OFFSET` for semantic pagination.

Every transaction runs `SELECT pg_is_in_recovery()` because a caller pool or proxy might route separate connections differently.

## 8. Commit Transaction Protocol

Before SQL, validate the complete commit request, aggregate byte/count bounds, duplicate keys, and canonical `RecordKey` ordering.

Inside one `SERIALIZABLE READ WRITE` transaction:

1. Set local statement timeout, lock timeout, and `synchronous_commit = on`.
2. Verify primary status.
3. Read the mutation ledger.
4. Return exact matching replay or hard mismatch before current-fence validation.
5. Lock the filesystem revision row.
6. Lock and validate the database-time writer fence.
7. Lock existing records in canonical semantic-key order.
8. Perform serializable predicate reads for required absent rows.
9. Validate every typed precondition.
10. Apply all normalized changes.
11. Allocate the next checked revision.
12. Insert the exact mutation result and whole-commit change event.
13. Commit once.

All changed records receive the same record revision. Preconditions that fail produce no mutation ledger row.

The per-filesystem revision row intentionally serializes the short publication phase and guarantees ordered change cursors. It does not serialize commits for unrelated filesystems.

## 9. Prepared Content

The adapter persists only portable `ContentRef` fields in the inode. `PublishContent` verifies the finalized state contract's preparation/inode/mutation/base/size/generation invariants before SQL.

Target data and manifests must already be durable. PostgreSQL never fetches or stores them. Per-block or per-extent PostgreSQL mappings are not part of this adapter; adding them requires a prior semantic contract change.

## 10. Retry and Error Classification

Use SQLSTATE and named constraints only:

```text
40001  serialization failure: definitive abort, bounded exact retry
40P01  deadlock: definitive abort, bounded exact retry and metric
23505  recognized uniqueness race: semantic conflict/ledger resolution
23503  recognized referential invariant failure
23514  recognized check/invariant failure
22003  numeric range failure
22P02  invalid textual numeric/input form
25006  readonly/standby routing failure
57014  timeout/cancellation classified by phase
08xxx  connection failure; ambiguous only when COMMIT may have executed
57P0x  server shutdown/availability failure classified by phase
```

Unknown constraints and codes are adapter failures rather than guessed semantic outcomes.

Known-abort retry repeats the identical transaction plan and remains bounded. It never changes the mutation ID, fingerprint, preconditions, or content preparation.

An error from `COMMIT` is potentially committed unless PostgreSQL definitively reports abort. Recovery opens a fresh primary transaction and resubmits the exact mutation request. Ledger-first replay resolves success; absence permits only that same request to execute. Recovery exhaustion returns explicit ambiguity.

## 11. Database-Time Leases and Fences

Lease operations use serializable transactions and PostgreSQL `clock_timestamp()` for deadline comparisons.

- Acquire locks the scope row and rejects another active holder.
- Fresh grant increments the greatest-ever numeric fence.
- Renew verifies the exact lease/holder/token and preserves the token.
- Release clears active fields but keeps the greatest token.
- Each operation is replayable through its stable lease-operation ID.
- Every non-replayed state commit validates an unexpired exact fence.
- Lease transitions participate in filesystem revisions and change polling.

No Rust process clock or advisory lock provides writer safety.

## 12. Durability Contract

The initial adapter advertises primary-WAL durability only. `open` verifies primary status, `fsync = on`, `full_page_writes = on`, permanent logged tables, and the ability to enforce transaction-local `synchronous_commit = on`.

The adapter does not advertise synchronous-standby failover durability. A later proposal may add a stronger validated mode using configured synchronous standbys. Hardware, backup, and failover quality remain deployment responsibilities.

## 13. Change Polling

Polling uses a primary serializable read transaction and stable revision keysets. It checks the oldest retained revision, reads a bounded number of whole event headers, fetches all keys for those events in ordinal order, and returns a resume revision plus `has_more`.

If history is missing, `RevisionCompacted` forces cache discard and authoritative reload. `LISTEN/NOTIFY` may be a future wake-up optimization but never the source of truth.

## 14. Testing

Offline tests verify numeric boundaries, key ordering, row conversion, config, migration checksums, and SQLSTATE mapping.

Live tests use explicit external DSNs and independently constructed pools. CI tests current PostgreSQL 15–18 minors. Tests cover migrations, primary/durability checks, shared conformance, transaction races, exact replay/mismatch, content publication, leases/fences, change gaps, lost commit acknowledgments, and fresh-client recovery.

No testcontainer or database provisioning dependency is introduced.
