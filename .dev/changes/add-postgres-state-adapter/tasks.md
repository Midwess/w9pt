# Tasks: Add PostgreSQL State Adapter

## Progress: [0/40]

### 1. Foundation and configuration

- [ ] 1.1 Block implementation until `add-filesystem-state-store` is approved and complete, then reconcile every adapter type and test against the finalized trait.
- [ ] 1.2 Add `crates/w9pt-fs-state-postgres` to the workspace with Rust 2024, Rust 1.85, Apache-2.0, documentation lints, `unsafe_code = "forbid"`, and workspace-compatible Clippy policy.
- [ ] 1.3 Add path dependency on `w9pt-fs-state` and exact SQLx `=0.8.6` with default features disabled and only PostgreSQL/Tokio runtime support; verify there is no `w9pt`, object SDK, or testcontainer dependency.
- [ ] 1.4 Define checked `PostgresStateConfig` for state limits, statement/lock timeouts, definitive-abort retries, ambiguous-commit recovery attempts, and primary-WAL durability.
- [ ] 1.5 Define the caller-owned `PgPool` `open`/`pool` API and advertise only the validated `SerializableMultiWriter` state-store contract.

### 2. Schema and migrations

- [ ] 2.1 Create the fixed fully qualified `w9pt_fs_state_v1` schema and adapter migration ledger with immutable embedded migration checksums.
- [ ] 2.2 Create filesystem, inode, directory-entry, open, orphan, and open-pin tables with named constraints and required indexes.
- [ ] 2.3 Create lock, xattr, and xattr-staging tables with checked ranges, bounded values, and inode-scoped indexes.
- [ ] 2.4 Create mutation-result, writer-fence, and lease-operation result tables with exact replay identities and retained greatest fence values.
- [ ] 2.5 Create normalized change-commit and ordered change-key tables with per-filesystem revision keys and explicit retained-history state.
- [ ] 2.6 Add fixed `BYTEA` widths, byte-count bounds, digest widths, enum domains, relationship checks, and full-range `NUMERIC(20,0)` constraints.
- [ ] 2.7 Implement explicit embedded migration execution under a fixed advisory lock; reject checksum drift/version gaps and never auto-migrate during `open`.

### 3. Conversion and startup validation

- [ ] 3.1 Implement canonical lossless `u64` to/from PostgreSQL numeric text conversion, including `0`, `i64::MAX`, `i64::MAX + 1`, and `u64::MAX`.
- [ ] 3.2 Implement canonical semantic `RecordKey` SQL ordering/encoding and reject duplicate affected keys before transaction work.
- [ ] 3.3 Implement exact domain-to-row and row-to-domain conversion for every authoritative record family and optional `ContentRef` field set.
- [ ] 3.4 Implement PostgreSQL 15–18 version, primary, schema/checksum, privilege, configured-limit, logged-table, `fsync`, `full_page_writes`, and synchronous-commit validation.
- [ ] 3.5 Implement SQLSTATE-plus-named-constraint classification without localized message matching.

### 4. Consistent reads

- [ ] 4.1 Implement common primary verification and transaction initialization for `SERIALIZABLE READ ONLY` and `SERIALIZABLE READ WRITE` operations.
- [ ] 4.2 Implement filesystem-revision and typed point queries while preserving `ReadBatch` request/result order.
- [ ] 4.3 Implement bounded directory, lock, xattr, open, orphan, lease, and mutation scans using stable keyset cursors rather than `OFFSET`.
- [ ] 4.4 Implement `LatestLinearizable` and `AtLeast` snapshots from one transaction revision and reject stale/standby results explicitly.

### 5. Atomic commit and recovery

- [ ] 5.1 Implement ledger-first exact replay and hard mismatch for changed mutation fingerprint, client incarnation, or retained result identity.
- [ ] 5.2 Lock the per-filesystem revision row, writer-fence row, and canonical existing record keys in deterministic order.
- [ ] 5.3 Implement serializable absent-key reads and typed record revision, generation, content-base, link-count, and open-pin preconditions.
- [ ] 5.4 Implement normalized insert/update/delete changes and complete cross-record invariant validation without cascade-hidden changes.
- [ ] 5.5 Implement prepared-content publication into inode `ContentRef` columns without storing payloads, manifests, or per-block metadata.
- [ ] 5.6 Atomically allocate one checked filesystem revision and persist record revisions, exact terminal result, change header, and ordered changed keys.
- [ ] 5.7 Implement bounded identical retries for definitive `40001` serialization and `40P01` deadlock aborts without semantic rebasing.
- [ ] 5.8 Implement ambiguous `COMMIT` recovery exclusively through bounded resubmission of the identical ledger-backed mutation request.

### 6. Leases and revision changes

- [ ] 6.1 Implement idempotent writer-scope acquire using PostgreSQL time and checked persistent fence increment.
- [ ] 6.2 Implement exact renew/release validation while preserving the greatest-ever fence after expiry or release.
- [ ] 6.3 Enforce exact active non-expired database-time fences inside every non-replayed commit.
- [ ] 6.4 Include lease transitions in per-filesystem revision and whole-commit change ordering.
- [ ] 6.5 Implement bounded revision keyset polling, resume cursors, `has_more`, and explicit compacted-history outcomes without correctness-critical `LISTEN/NOTIFY`.

### 7. Verification and documentation

- [ ] 7.1 Add offline tests for numeric boundaries, byte/name codecs, record-key ordering, row codecs, configuration, migration checksums, and SQLSTATE classification.
- [ ] 7.2 Run the reusable state-store conformance suite through independently constructed pools and fresh adapter instances against current PostgreSQL 15, 16, 17, and 18 minors.
- [ ] 7.3 Test atomic namespace, prepared-content, open/orphan, lock, xattr, conflict, retry, and exact mutation-replay behavior.
- [ ] 7.4 Inject before-commit and after-commit/lost-ack failures and prove exact retry exposes one complete committed result.
- [ ] 7.5 Test database-time expiry, renewal, release, reacquisition, stale writers, monotonic fences, and concurrent lease contenders.
- [ ] 7.6 Document roles, migrations, DSNs, TLS/pool ownership, primary-WAL durability, unsupported features, and test commands; run workspace tests, formatting, Clippy with warnings denied, rustdoc, dependency checks, migration validation, standby rejection, durability validation, change polling, and fresh-client recovery.

---

## Notes

- Implementation is blocked while `add-filesystem-state-store` remains draft or incomplete.
- PostgreSQL stores authoritative records and inode `ContentRef` fields, not bulk content or block mappings.
- Synchronous-standby durability and replica reads require separate proposals and are not advertised by version 1.
- Live database tests use explicit environment-provided DSNs; the repository does not provision containers or databases.
