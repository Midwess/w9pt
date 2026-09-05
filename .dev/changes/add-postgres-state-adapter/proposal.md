# Add PostgreSQL Filesystem State Adapter

Status: approved

## Summary

Add `w9pt-fs-state-postgres`, a PostgreSQL 15–18 adapter implementing the finalized `w9pt-fs-state::FilesystemStateStore` contract over a caller-owned SQLx `PgPool`. The adapter provides primary-authoritative one-snapshot reads, serializable multi-record commits, ledger-first mutation replay, database-time leases with monotonic fences, bounded revision polling, explicit embedded migrations, durability validation, and the shared state-store conformance suite.

PostgreSQL stores normalized filesystem metadata and coordination records. Immutable file payloads and manifests remain in target storage; the inode stores only the authoritative portable `ContentRef`. Per-block PostgreSQL mappings require a separate state/content-index contract change.

## Dependency Reconciliation

The dependency gate is satisfied. `add-filesystem-state-store` is approved and complete at `37/37`, `crates/w9pt-fs-state` exists, and its public trait, records, limits, outcomes, lease protocol, change protocol, and conformance harness are the implementation source of truth.

The adapter design has been reconciled against that implementation. In particular:

- a private `authority_heads` row owns revision allocation independently of the optional public `FilesystemRecord`, preserving lease-before-filesystem bootstrap;
- every point query, scan cursor, semantic key, outcome, and replay check follows the finalized public types exactly;
- lease ticks are fixed to unsigned microseconds, stored as checked `NUMERIC(20,0)`, and derived from one PostgreSQL clock observation per transaction;
- mutation replay uses `MutationContext::classify_record` and introduces no adapter-specific mismatch dimensions;
- change polling returns the exact `ChangePollOutcome` variants and `ChangeBatch` fields rather than an adapter-only `has_more` field;
- a database-resident, test-only manual clock lets independent PostgreSQL clients run the reusable conformance suite deterministically.

## Motivation

The distributed 9P design needs an authoritative control plane that remains correct when processing nodes are disposable. PostgreSQL can execute short serializable transactions across inode, namespace, open, orphan, lock, lease, mutation-result, and revision records while allowing independently pooled clients.

A dedicated adapter crate provides:

- atomic namespace and inode transitions using native database transactions;
- exact durable mutation replay after process or connection failure;
- primary-authoritative reads with consistent multi-query snapshots;
- persistent leases and fencing independent of processing-node memory;
- stable ordered state scans and change polling;
- explicit mapping between portable state types and a versioned relational schema;
- a reusable conformance target before integration with the future filesystem engine.

## Goals

- Introduce `crates/w9pt-fs-state-postgres` without changing `w9pt-fs-state`, `w9pt-fs-storage`, or `w9pt` public formats.
- Implement `FilesystemStateStore` with `WriterTopology::SerializableMultiWriter` and the complete production guarantee set.
- Accept a caller-created `sqlx::PgPool`; keep credentials, URLs, TLS roots, pool sizing, and runtime lifecycle caller-owned.
- Use SQLx exactly `0.8.6`, compatible with Rust 1.85, with default features disabled.
- Support current PostgreSQL 15, 16, 17, and 18 minor releases using PostgreSQL 15-compatible SQL.
- Use the fixed fully qualified `w9pt_fs_state_v1` schema and explicit embedded checksummed migrations.
- Separate private authority revision state from the public filesystem record.
- Store every finalized record family and implement every finalized point query and scan cursor losslessly.
- Preserve full public unsigned ranges with checked `NUMERIC(20,0)` decimal conversion.
- Execute authoritative reads and writes only on a verified writable primary.
- Follow the finalized ledger-first commit protocol, including replay before adapter-limit and fence validation.
- Treat an uncertain `COMMIT` response as potentially committed and recover only through exact mutation replay.
- Use PostgreSQL time for production lease expiry and permanently retain the greatest fence per scope.
- Return exact read, commit, lease, and change-poll semantic outcomes without database-specific leakage.
- Run offline tests plus the shared state-store conformance suite through independent PostgreSQL pools.

## Scope

### In scope

- PostgreSQL adapter package metadata and runtime-specific dependencies.
- Exact SQLx `=0.8.6` with PostgreSQL and Tokio runtime support.
- Caller-owned `PgPool`, checked adapter configuration, bounded retry/recovery policy, and explicit `open`/`migrate` APIs.
- Fixed-schema normalized tables, indexes, named checks, foreign keys, migration ledger, and migration checksums.
- A private lazy `authority_heads` table with revision-one empty-authority semantics.
- A separate public `filesystem_records` table containing every `FilesystemRecord` field.
- Checked codecs for fixed IDs, byte-preserving names, digests, object keys, enums, all record variants, and full-range `u64` values.
- Primary-routing validation at startup and inside every authoritative transaction, plus explicit writeability validation for write transactions.
- Serializable read batches, all finalized point queries, all ten finalized scan families, and exact read outcomes.
- Ledger-first serializable commits, typed preconditions/changes, deterministic semantic locks, revision allocation, result persistence, and whole-commit events.
- Prepared-content and xattr-staging publication through their dedicated finalized transitions.
- Known-abort retries and exact ambiguous-commit recovery.
- Idempotent database-time lease acquire/renew/release, exact operation replay, and monotonic fencing.
- Exact bounded change-poll outcomes, including future cursors, compacted history, and an event too large for the requested key bound.
- Offline tests and environment-driven PostgreSQL 15–18 live tests.
- Test-only database-resident manual clock support for deterministic conformance.
- Documentation for roles, migrations, durability, clock units, test DSNs, and unsupported deployment guarantees.

### Out of scope

- Changes to the finalized `w9pt-fs-state` contract.
- Changes to `w9pt`, protocol messages, session state, effects, completions, or capability negotiation.
- File payloads, S3 manifest objects, block objects, packed segments, or target-object operations.
- PostgreSQL per-block or per-extent content mappings.
- SQLite, etcd, SlateDB, Redis, or other adapters.
- Replica reads, multi-primary PostgreSQL, or cross-region active/active routing.
- `LISTEN/NOTIFY` as a correctness mechanism.
- Automatic migrations during adapter construction.
- Database/role creation, credentials, TLS certificates, pool sizing, backups, failover orchestration, monitoring, or deployment automation.
- Testcontainers, Docker orchestration, or a mandatory local PostgreSQL installation.
- Mutation-ledger pruning beyond the finalized retention and bounded-history contract.
- Filesystem policy execution, permission checks, semantic rebasing, or errno mapping.

## Affected Areas

| Area | Impact |
|---|---|
| `Cargo.toml` / `Cargo.lock` | Add the adapter workspace member and SQLx graph |
| `crates/w9pt-fs-state-postgres` | New adapter, migrations, codecs, SQL mapping, test support, and tests |
| `crates/w9pt-fs-state` | Dependency only; no contract changes |
| `w9pt` / `w9pt-fs-storage` | No source, wire, or persistent-format changes |
| `README.md` | Document PostgreSQL guarantees and live-test commands |
| `.gitattributes` | Keep embedded migration SQL on canonical LF line endings |
| `.dev/project.md` | Record reconciled adapter conventions and durability boundary |

## Dependencies

- Approved and completed `add-filesystem-state-store` and its stable conformance API.
- Rust 2024 with the workspace Rust 1.85 baseline.
- SQLx exactly `0.8.6`, default features disabled, PostgreSQL and Tokio runtime enabled.
- Caller-selected compatible SQLx TLS feature through Cargo feature unification when required.
- PostgreSQL 15, 16, 17, or 18 at its current supported minor release.
- A caller-created `PgPool` whose acquired connections route to a writable primary.
- A migration role permitted to create the fixed schema and a runtime role limited to required DML, row locking, and transaction settings.
- Explicit environment DSNs for live CI.

## Risks

| Risk | Mitigation |
|---|---|
| Private revision state is confused with a public filesystem record | Use separate `authority_heads` and `filesystem_records` tables; test lease-before-bootstrap and absent filesystem reads |
| A hot filesystem revision head limits write throughput | Keep the head per filesystem and publication transactions short; benchmark before claiming throughput |
| PostgreSQL `BIGINT` narrows public `u64` values | Use constrained `NUMERIC(20,0)` and canonical checked decimal codecs |
| Timestamp storage narrows exact lease ticks | Persist integer microsecond ticks, not `TIMESTAMPTZ`; derive production time once from PostgreSQL |
| Database time makes conformance nondeterministic | Use a test-only database clock row shared by independent pools and separately test the production clock conversion |
| Scan order drifts from Rust `RecordScan` cursors | Encode every finalized cursor explicitly and test SQL ordering against Rust ordering |
| Serializable abort is mistaken for a semantic outcome | Classify exact SQLSTATE and rerun only the identical bounded transaction |
| A lost `COMMIT` response causes duplicate mutation | Return ambiguity or resolve through the finalized ledger-first replay path only |
| Constraint failures leak database behavior | Map only named known constraints; treat unknown violations as adapter invariant failures |
| A proxy selects a standby or write-disabled primary | Check recovery on every transaction, the connection's default read-only state, and actual write-transaction state |
| PostgreSQL settings weaken durability | Validate server settings and enforce transaction-local `synchronous_commit = 'on'` |
| Migration drift affects rolling nodes | Use a fixed schema, immutable checksums, an advisory migration lock, explicit migration, and fail-closed `open` |
| Runtime statements depend on search path or collation | Fully qualify fixed identifiers and store semantic byte strings as `BYTEA` |
| Database tests silently skip | Make required CI mode fail when declared version DSNs are missing |

## Verified References

- PostgreSQL serializable transactions may abort with `serialization_failure`, requiring complete transaction retry: <https://www.postgresql.org/docs/18/sql-set-transaction.html>
- PostgreSQL timestamp values have microsecond resolution, which is why lease deadlines use integer microsecond ticks: <https://www.postgresql.org/docs/18/datatype-datetime.html>
- PostgreSQL synchronous replication and `synchronous_commit` define distinct local/standby durability boundaries: <https://www.postgresql.org/docs/current/warm-standby.html>
- PostgreSQL 15–18 are supported and PostgreSQL 19 remains prerelease as of this reconciliation: <https://www.postgresql.org/support/versioning/>
- SQLx 0.8.6 supports the workspace baseline while the SQLx 0.9 line raises MSRV to Rust 1.86: <https://github.com/launchbadge/sqlx/blob/main/CHANGELOG.md>
