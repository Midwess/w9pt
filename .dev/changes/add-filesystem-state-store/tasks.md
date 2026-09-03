# Tasks: Add Filesystem State Store

## Progress: [0/37]

### 1. Foundations

- [ ] 1.1 Add `crates/w9pt-fs-state` to the workspace with Rust 2024 metadata, Apache-2.0 licensing, documentation lints, `unsafe_code = "forbid"`, and workspace-compatible Clippy policy.
- [ ] 1.2 Add only the path dependency on `w9pt-storage`; verify there is no `w9pt`, SDK, database-client, or async-runtime dependency.
- [ ] 1.3 Define fixed-width filesystem, inode, open, lock, lease, writer-scope, writer-incarnation, and client-incarnation IDs.
- [ ] 1.4 Define checked monotonic state/record revisions, directory cookies, fencing tokens, timestamps/deadlines, and state request fingerprints.
- [ ] 1.5 Define validated `StateLimits` for names, xattrs, result bytes, read queries, scan pages, preconditions, changes, change events, lease duration, and aggregate transaction size.

### 2. Authoritative records

- [ ] 2.1 Define bounded entry names, xattr names, principal/group identities, symlink targets, and opaque versioned mutation-result bytes.
- [ ] 2.2 Define validated inode kinds and `InodeRecord`, including mode, ownership, times, size, link count, inode/data generations, explicit content-file identity, and optional `ContentRef`.
- [ ] 2.3 Define `FilesystemRecord`, root invariants, `DirectoryEntryRecord`, directory generations, and stable non-reused directory cookies.
- [ ] 2.4 Define portable `OpenRecord`, `OrphanRecord`, and durable open-pin representation.
- [ ] 2.5 Define byte-range `LockRecord`, lock ownership, EOF-range semantics, and checked arithmetic.
- [ ] 2.6 Define `XattrRecord`, `XattrStagingRecord`, `MutationRecord`, and `WriterLeaseRecord`.
- [ ] 2.7 Define `RecordKey`/`StateRecord`, exact key/value variant matching, and cross-record invariant validators.

### 3. Contract and reads

- [ ] 3.1 Define `WriterTopology`, required store guarantees, and validated `StateStoreContract` without weakening guarantees for any adapter.
- [ ] 3.2 Define bounded typed point and ordered-range `ReadQuery` variants for every authoritative record family.
- [ ] 3.3 Define `ReadBatch`, `ReadConsistency`, positionally matched `ReadResult`, and one-revision `StateSnapshot`.
- [ ] 3.4 Define typed adapter failures separately from semantic read, commit, lease, and revision outcomes.
- [ ] 3.5 Define the runtime-neutral `FilesystemStateStore` trait with owned requests and return-position futures.

### 4. Commit protocol

- [ ] 4.1 Define `MutationContext`, client incarnation, retention horizon, exact terminal `MutationResult`, and committed-result replay types.
- [ ] 4.2 Define typed record, inode-generation, directory-generation, content-base, link-count, open-pin, and exact-fence preconditions.
- [ ] 4.3 Define bounded insert, replace, delete, and checked counter/generation changes for every authoritative record family.
- [ ] 4.4 Define special `PublishContent` behavior and validate prepared mutation, content-file identity, base, logical size, and generation binding.
- [ ] 4.5 Define committed, already-committed, conflict, mutation-mismatch, stale-fence, expired-lease, malformed-request, and ambiguous outcomes.
- [ ] 4.6 Implement complete commit preflight validation so invalid batches cannot partially act or allocate beyond configured bounds.
- [ ] 4.7 Document and test the normative ledger-first replay check and atomic records/result/change-event commit order.

### 5. Lease and revision protocol

- [ ] 5.1 Define idempotent acquire, renew, and release request/outcome types with stable operation identities and bounded durations.
- [ ] 5.2 Define `WriterFence` and exact lease, scope, holder, token, and expiry validation.
- [ ] 5.3 Define an explicit lease-time authority and deterministic manual clock for tests without reading a process-global clock.
- [ ] 5.4 Define monotonic fence allocation, expiry, renewal, release, takeover, and single-writer topology rules.
- [ ] 5.5 Define bounded whole-commit change events, polling cursors, empty batches, retention, and compacted-revision outcomes.

### 6. Memory authority and conformance

- [ ] 6.1 Implement a deterministic shared memory authority and independently opened cloneable clients without correctness-bearing client caches.
- [ ] 6.2 Implement consistent batch reads and bounded ordered scans under one locked authoritative revision.
- [ ] 6.3 Implement staged atomic commits, record revisions, durable-in-model result replay, and whole-commit change-log emission.
- [ ] 6.4 Implement deterministic lease operations, manual expiry, monotonic fencing, and writer-topology enforcement.
- [ ] 6.5 Add before-commit and after-commit ambiguous failure injection plus ordered operation tracing.
- [ ] 6.6 Add reusable adapter conformance covering reads, atomicity, idempotency, fencing, change polling, independent-client reopen, and configured bounds.
- [ ] 6.7 Test create, rename, link/unlink, prepared-content publication, open-unlinked pins, locks, xattrs, and simultaneous attribute fields as atomic record sets.
- [ ] 6.8 Document SQLite, PostgreSQL, etcd, and SlateDB adapter obligations; run workspace tests, formatting, Clippy with warnings denied, rustdoc, and dependency-tree checks.

---

## Notes

- Production adapters and the filesystem semantic engine require separate approved changes.
- The deterministic memory authority is a semantic reference and does not independently justify advertising production `DurableMetadata`.
- Physical adapter keys, tables, indexes, encodings, and migrations remain private and versioned by each adapter.
