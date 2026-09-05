# Tasks: Add PostgreSQL State Adapter

## Progress: [47/47]

### 1. Foundation and configuration

- [x] 1.1 Reconcile the adapter against the approved, completed `w9pt-fs-state` trait, records, outcomes, cursor types, protocol order, and conformance harness; approve this implementation proposal.
- [x] 1.2 Add `crates/w9pt-fs-state-postgres` to the workspace with Rust 2024, Rust 1.85, Apache-2.0, documentation lints, `unsafe_code = "forbid"`, and workspace-compatible Clippy policy.
- [x] 1.3 Add the path dependency on `w9pt-fs-state` and exact SQLx `=0.8.6` with default features disabled and PostgreSQL/Tokio support; verify there is no `w9pt`, object SDK, or testcontainer dependency.
- [x] 1.4 Define checked `PostgresStateConfig` for finalized state limits, statement/lock timeouts, definitive-abort retries, ambiguous-commit recovery attempts, and primary-WAL durability.
- [x] 1.5 Define caller-owned `PgPool` `open`/`pool` APIs, explicit `migrate`, production database-clock construction, and the validated `SerializableMultiWriter` production contract.

### 2. Schema and migrations

- [x] 2.1 Create the fixed fully qualified `w9pt_fs_state_v1` schema and adapter migration ledger with immutable embedded checksums.
- [x] 2.2 Create private `authority_heads` with revision-one empty-authority semantics, separately create public `filesystem_records` with every finalized field, and support lease-before-filesystem bootstrap.
- [x] 2.3 Create inode, directory-entry, open, orphan, and open-pin tables with exact finalized keys, named constraints, and required indexes.
- [x] 2.4 Create lock, xattr, and xattr-staging tables using exact `(filesystem, inode, lock)` and `(filesystem, inode, name)` identities plus bounded range/conflict indexes.
- [x] 2.5 Create mutation-result, writer-fence, and lease-operation tables with finalized replay fields, nullable active lease state, and permanently retained greatest fence values.
- [x] 2.6 Create normalized change-commit and ordered change-key tables with tagged mutation/lease origins, per-filesystem revisions, key counts, and retained-history state.
- [x] 2.7 Add fixed byte widths, byte-count bounds, enum tags, optional-`ContentRef` all-or-none checks, relationship checks, and full-range `NUMERIC(20,0)` constraints.
- [x] 2.8 Implement transactional embedded migration execution under the fixed advisory lock; reject checksum drift/version gaps and never auto-migrate during `open`.

### 3. Conversion and startup validation

- [x] 3.1 Implement canonical checked `u64` to/from numeric text conversion, including `0`, `i64::MAX`, `i64::MAX + 1`, and `u64::MAX`, with no floating-point or signed narrowing.
- [x] 3.2 Implement canonical finalized `RecordKey::Ord` SQL tags/components and reject duplicate affected keys before transactional state work.
- [x] 3.3 Implement exact domain-to-row and row-to-domain conversion for every `StateRecord`, including complete filesystem fields and optional `ContentRef` reconstruction.
- [x] 3.4 Implement PostgreSQL 15–18 version, writable-primary, schema/checksum, privilege, configured-limit, logged-table, `fsync`, `full_page_writes`, and synchronous-commit validation.
- [x] 3.5 Implement SQLSTATE-plus-phase-plus-named-constraint classification without localized message matching.

### 4. Consistent reads

- [x] 4.1 Implement common transaction setup and per-transaction writable-primary checks for `SERIALIZABLE READ ONLY` and `SERIALIZABLE READ WRITE` operations.
- [x] 4.2 Implement private authority-head revision reads, revision-one absence behavior, and every finalized point query while preserving `ReadBatch` result order.
- [x] 4.3 Implement all ten finalized scans with exact cursors: inode; directory cookie; open; `(inode, open)` pin; orphan; `(inode, lock)` lock; `(inode, name)` xattr; staging; mutation; and active writer lease.
- [x] 4.4 Stream/decode scan results within item/byte bounds and return exact `Snapshot`, `RevisionUnavailable`, `MalformedRequest`, and `ScanBoundTooSmall` outcomes.
- [x] 4.5 Test SQL keyset ordering and resume behavior against finalized Rust cursor ordering; use no semantic `OFFSET` query.

### 5. Atomic commit and recovery

- [x] 5.1 Implement the short primary serializable fixed-size ledger probe before adapter-limit/fence validation and classify retained records only through `MutationContext::classify_record`.
- [x] 5.2 After an absent probe, run finalized adapter-limit preflight outside the write transaction and repeat ledger lookup first inside every write attempt.
- [x] 5.3 Lazily create/lock the private authority head, lock the writer fence, then lock existing semantic rows in canonical `RecordKey` order.
- [x] 5.4 Implement serializable absent-key predicate reads and every finalized record-revision, generation, content-base, link-count, open-pin, exact-fence, and lock-conflict precondition.
- [x] 5.5 Implement every finalized insert/replace/delete/counter/generation change with targeted cross-record invariants and no cascade-hidden mutation or unbounded whole-filesystem load.
- [x] 5.6 Implement dedicated prepared-content and xattr-staging publication exactly as finalized, without target-object I/O or publication bypasses.
- [x] 5.7 Atomically allocate one checked authority revision and persist public record revisions, exact mutation result, one change header, and ordered changed keys including the mutation key.
- [x] 5.8 Implement bounded identical retries for definitive `40001` serialization and `40P01` deadlock aborts without semantic rebasing.
- [x] 5.9 Implement uncertain-`COMMIT` recovery through a fresh primary ledger probe and exact request resubmission, returning finalized `CommitOutcome::Ambiguous` on exhaustion.

### 6. Leases, clocks, and revision changes

- [x] 6.1 Define version-1 lease ticks as microseconds; persist deadlines as `NUMERIC(20,0)` and capture one checked PostgreSQL `clock_timestamp()`-derived tick per production transaction.
- [x] 6.2 Implement acquire with lease-operation replay before validation, checked duration/deadline arithmetic, active-scope locking, and permanent fence increment.
- [x] 6.3 Implement exact renew/release replay and validation, never shorten renewal, clear only active fields on release, and preserve the greatest token.
- [x] 6.4 Enforce exact current non-expired database-time fences inside every non-replayed commit.
- [x] 6.5 Allocate revisions/change events only for successful grant, renew, and release transitions; retain exact required rejection outcomes without fake public events.
- [x] 6.6 Implement exact change polling outcomes: `Changes`, `RevisionUnavailable`, `RevisionCompacted`, `PollBoundTooSmall`, and `MalformedRequest`, using `next/current_revision` with no adapter-only `has_more` field.
- [x] 6.7 Add feature-gated test support using a separate database-resident manual tick row so independent pools implement deterministic conformance `advance_time` without shared process RAM.

### 7. Verification and documentation

- [x] 7.1 Add offline tests for numeric boundaries, clock conversion, byte/name codecs, every record row codec, key ordering, scan mapping, configuration, migration checksums, and SQLSTATE classification.
- [x] 7.2 Run the reusable state-store conformance suite through independent pools and fresh adapter instances against current PostgreSQL 15, 16, 17, and 18 minors.
- [x] 7.3 Test empty revision-one reads, lease acquisition before public filesystem creation, bootstrap `RecordAbsent`, and public/private filesystem-state separation.
- [x] 7.4 Test atomic namespace, prepared-content, xattr-staging, open/orphan, lock, conflict, exact mutation replay, and every point/scan family.
- [x] 7.5 Inject before-publication and post-commit/lost-ack failures and prove exact replay exposes one complete committed result.
- [x] 7.6 Test database-time conversion, deterministic test-clock advance, expiry, renewal, release, takeover, stale writers, monotonic fences, and concurrent contenders.
- [x] 7.7 Test all semantic outcome variants, change-poll future/compacted/too-small boundaries, history limits, standby/read-only rejection, durability validation, and fresh-client recovery.
- [x] 7.8 Document roles, migrations, DSNs, TLS/pool ownership, microsecond tick semantics, primary-WAL durability, unsupported features, and test commands; run workspace tests, formatting, Clippy with warnings denied, rustdoc, and dependency checks.

---

## Notes

- The prerequisite is complete and the reconciled proposal is approved; implementation may begin with Task 1.2.
- PostgreSQL stores authoritative records and inode `ContentRef` fields, not bulk content or block mappings.
- Synchronous-standby durability, replica reads, and production notification streams require separate proposals.
- Live database tests use explicit environment-provided DSNs; the repository does not provision containers or databases.
- The reusable conformance suite passed on refreshed PostgreSQL 15, 16, 17, and 18 Alpine images with required-mode DSNs on 2026-09-04.
- Final workspace tests, formatting, Clippy with warnings denied, rustdoc with warnings denied, and dependency-isolation checks passed.
- Post-implementation code-review findings were resolved with tighter-limit replay, staged namespace/lock/dependency, ambiguity-recovery, monotonic-history, timeout, migration-constraint, and independent-pool regression coverage.
