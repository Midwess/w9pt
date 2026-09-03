# Add PostgreSQL Filesystem State Adapter

Status: draft

## Summary

Add `w9pt-fs-state-postgres`, a PostgreSQL 15–18 adapter implementing the finalized `w9pt-fs-state::FilesystemStateStore` contract over a caller-owned SQLx `PgPool`. The adapter will provide primary-authoritative one-snapshot reads, serializable multi-record commits, ledger-first mutation replay, database-time leases with monotonically increasing fences, bounded revision polling, explicit embedded schema migrations, durability validation, and the shared state-store conformance suite.

This proposal stores normalized filesystem metadata and coordination records in PostgreSQL. Immutable file payloads and block-split manifests remain in target storage and the inode stores the authoritative `ContentRef`. Per-block PostgreSQL mappings require a separate state/content-index contract change and are intentionally not introduced by this adapter.

## Dependency Gate

Implementation is blocked until `add-filesystem-state-store` is approved and implemented. That proposal is currently draft and `crates/w9pt-fs-state` does not yet exist. The PostgreSQL schema must be derived from the finalized public IDs, records, limits, read queries, commit changes, lease types, outcomes, and conformance factory rather than duplicating or anticipating them.

## Motivation

The distributed 9P design needs an authoritative control plane that remains correct when processing nodes are disposable. PostgreSQL is a suitable first clustered adapter because it can execute short serializable transactions across inode, namespace, open, orphan, lock, lease, mutation-result, and revision records while allowing independently pooled clients.

A dedicated adapter crate provides:

- atomic namespace and inode transitions using native database transactions;
- exact durable mutation replay after process or connection failure;
- primary-authoritative reads with consistent multi-query snapshots;
- persistent leases and fencing independent of a processing node's memory;
- stable ordered directory and change-feed pagination;
- explicit mapping between portable state types and a versioned relational schema;
- a reusable conformance target before integrating the future filesystem engine.

## Goals

- Introduce `crates/w9pt-fs-state-postgres` without changing `w9pt` or `w9pt-storage` formats.
- Implement the finalized `FilesystemStateStore` trait with `WriterTopology::SerializableMultiWriter`.
- Accept a caller-created `sqlx::PgPool`; keep credentials, URLs, TLS roots, pool sizing, and runtime lifecycle caller-owned.
- Use SQLx exactly `0.8.6`, compatible with the workspace Rust 1.85 baseline; SQLx 0.9 requires Rust 1.86.
- Support current maintained PostgreSQL 15, 16, 17, and 18 minor releases using PostgreSQL 15-compatible SQL.
- Use a fixed, fully qualified `w9pt_fs_state_v1` schema and explicit embedded checksummed migrations.
- Store normalized authoritative records with database constraints duplicating public model bounds and invariants.
- Preserve the full public unsigned ranges through checked `NUMERIC(20,0)` decimal conversion rather than unchecked `BIGINT` casts.
- Execute read batches in primary-only `SERIALIZABLE READ ONLY` transactions.
- Execute commits and lease mutations in short primary-only `SERIALIZABLE READ WRITE` transactions.
- Check the mutation ledger before current fence validation and return exact retained results for matching retries.
- Lock affected semantic record keys in deterministic order and classify SQLSTATE/constraint outcomes without localized message matching.
- Treat a lost or failed `COMMIT` response as potentially committed and recover only by retrying the identical mutation identity.
- Use PostgreSQL time for lease expiry, retain highest-ever fence values, and reject stale writers.
- Implement bounded keyset change polling with explicit compacted-revision outcomes.
- Validate primary routing, schema version, privileges, `fsync`, `full_page_writes`, and primary-WAL synchronous commit semantics before advertising the contract.
- Run the shared state-store conformance suite through independently constructed pools and clients.

## Scope

### In scope

- PostgreSQL adapter package metadata and runtime-specific dependencies.
- Exact SQLx `=0.8.6` with default features disabled and PostgreSQL/Tokio support.
- Caller-owned `PgPool`, validated adapter configuration, bounded retry/recovery policy, and explicit `open`/`migrate` APIs.
- Fixed-schema normalized tables, indexes, checks, foreign keys, migration ledger, and migration checksums.
- Checked codecs for fixed-width IDs, byte-preserving names, digests, object keys, enums, records, and full-range `u64` values.
- Primary-only startup and per-transaction validation.
- Serializable read batches and bounded keyset scans.
- Ledger-first serializable commits, typed preconditions/changes, deterministic locks, revision allocation, result persistence, and change events.
- Prepared-content publication into inode `ContentRef` columns only.
- Known-abort retries for serialization/deadlock failures without semantic rebasing.
- Exact ambiguous-commit recovery through mutation-ledger replay.
- Idempotent database-time lease acquire/renew/release and monotonic fencing.
- Bounded change polling and explicit history-gap results.
- Offline codec/config/error tests and live PostgreSQL 15–18 integration/conformance tests using environment-provided DSNs.
- Documentation for deployment guarantees, privileges, migration roles, durability boundaries, and test commands.

### Out of scope

- Implementing or modifying the still-draft `w9pt-fs-state` contract.
- Changes to `w9pt`, protocol messages, session state, effects, completions, or capability negotiation.
- File payloads, S3 manifests, block objects, packed segments, or target-object operations.
- PostgreSQL per-block or per-extent content mappings.
- SQLite, etcd, SlateDB, Redis, or other adapters.
- Replica reads, follower-read consistency, multi-primary PostgreSQL, or cross-region active/active routing.
- `LISTEN/NOTIFY` as a correctness mechanism or required wake-up stream.
- Automatic migrations during adapter construction.
- Database creation, user/role creation, TLS certificates, secrets, connection URLs, pool sizing, backup, failover orchestration, monitoring, or deployment automation.
- Testcontainers, Docker orchestration, or a mandatory local PostgreSQL installation.
- Mutation-ledger pruning beyond enforcement of the finalized state contract's retention and bounds.
- Filesystem semantic execution, permission checks, retry policy above state conflicts, or errno mapping.

## Affected Areas

| Area | Impact |
|---|---|
| `Cargo.toml` / `Cargo.lock` | Add adapter workspace member and SQLx dependency graph |
| `crates/w9pt-fs-state-postgres` | New adapter, schema migrations, codecs, SQL transaction mapping, and tests |
| `crates/w9pt-fs-state` | Dependency only after its proposal is implemented; no contract changes in this proposal |
| `w9pt` / `w9pt-storage` | No source, wire, content-format, or dependency changes |
| `README.md` | Document PostgreSQL adapter, guarantees, configuration ownership, and live test commands |
| `.gitattributes` | Ensure migration SQL has canonical LF line endings if not already covered |
| `.dev/project.md` | Record adapter conventions, supported versions, and durability boundary |

## Dependencies

- Approved and completed `add-filesystem-state-store` with stable public trait and conformance support.
- Rust 2024 with the workspace Rust 1.85 baseline.
- SQLx exactly `0.8.6`; default features disabled, PostgreSQL and Tokio runtime enabled.
- Caller-selected SQLx TLS feature through Cargo feature unification when remote TLS is required.
- PostgreSQL 15, 16, 17, or 18 at its current supported minor release.
- A caller-created `PgPool` connected directly or through routing that always selects a writable primary.
- A migration role permitted to create the fixed schema/tables and a separate runtime role permitted only the required DML/sequence operations.
- Live CI databases supplied through explicit environment DSNs.

## Risks

| Risk | Mitigation |
|---|---|
| State trait changes invalidate the schema | Keep the proposal draft and block Task 1.1 until the state contract is implemented and reconciled |
| Per-filesystem revision row becomes a write hotspot | Keep transactions short, serialize only within one filesystem, benchmark contention, and defer sharded revision domains |
| PostgreSQL `BIGINT` truncates public `u64` values | Use constrained `NUMERIC(20,0)` with checked canonical decimal codecs |
| Collation changes namespace ordering or uniqueness | Store names as bounded `BYTEA` and use explicit keyset ordering |
| Serializable abort is mistaken for semantic success/failure | Classify exact SQLSTATE and rerun the identical transaction only within a bounded known-abort retry policy |
| Connection loss during `COMMIT` causes duplicate mutation | Treat it as ambiguous and resolve only through ledger-first retry of the exact mutation request |
| Unique/FK/check violations leak database behavior | Map only named known constraints; unknown violations are adapter invariant errors |
| Standby or proxy routing serves stale/readonly state | Verify `pg_is_in_recovery() = false` inside every authoritative transaction |
| PostgreSQL configuration weakens durability | Validate server settings and force transaction-local `synchronous_commit = 'on'` before advertising primary-WAL durability |
| Schema migration drifts across rolling nodes | Fixed schema, embedded immutable checksums, advisory migration lock, explicit migration API, and fail-closed `open` |
| Dynamic SQL identifiers permit injection or search-path changes | Use a fixed quoted schema and fully qualify every runtime object |
| Long/unbounded query plans retain locks and memory | Validate public and SQL bind/result bounds before SQL; use keyset scans and deterministic record locking |
| Database tests silently do not run | CI sets a required flag and explicit version-specific DSNs; local omission reports an intentional skip |
| PostgreSQL is mistaken for content storage | Keep payloads, manifests, and block mappings outside this crate and document the `ContentRef`-only boundary |

## Verified References

- PostgreSQL serializable transactions may abort with `serialization_failure`, requiring complete transaction retry: <https://www.postgresql.org/docs/18/sql-set-transaction.html>
- PostgreSQL synchronous replication and `synchronous_commit` define distinct local/standby durability boundaries: <https://www.postgresql.org/docs/current/warm-standby.html>
- PostgreSQL 15–18 are currently supported; PostgreSQL 14 reaches end of support in November 2026: <https://www.postgresql.org/support/versioning/>
- SQLx 0.8.6 supports the workspace MSRV while SQLx 0.9 raises MSRV to Rust 1.86: <https://github.com/launchbadge/sqlx/blob/main/CHANGELOG.md>
