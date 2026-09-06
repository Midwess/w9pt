# Tasks: add-content-compression-encryption

## Progress: [41/41]

## 1. Contracts and design gate

- [x] 1.1 Finalize per-file context/DEK ownership, one external master KEK, generic opaque state records, retained-key lifetime and the revised cross-layer API boundary.
- [x] 1.2 Freeze current v3 content and wrapped-key formats, stable bindings/commitments, KDF/AAD/token inputs, LZ4 profile and resource equations.
- [x] 1.3 Validate exact optional dependencies, Rust 1.94.1, licenses/advisories, encoder reproducibility and cipher cleanup; keep entropy/runtime services caller-owned.

## 2. Generic state metadata

- [x] 2.1 Add bounded ContentMetadata record/key/family types, stable owner/context/policy/key-commitment fields and regular-inode context binding.
- [x] 2.2 Add exact context reads, composite inode-with-context snapshot reads, bounded keyset scans/cursors, record-byte accounting and revision change events.
- [x] 2.3 Enforce atomic context/inode creation and immutable owner/context/policy/commitment, rejecting generic replacement/deletion bypasses.
- [x] 2.4 Extend PreparedContent/publication validation with nonsecret context/key binding, exact bounded policy-format/byte equality and context revision checks.
- [x] 2.5 Add dedicated revision/fence/ledger-protected RewrapContentMetadata that changes only the wrapped envelope.
- [x] 2.6 Extend the memory reference and generic opaque conformance for creation, binding, rewrap, replay and context survival after inode retirement.

## 3. File-key cryptography

- [x] 3.1 Add caller-owned secure entropy and master-key interfaces; generate one candidate random DEK and return only its bounded wrapped metadata.
- [x] 3.2 Implement authenticated wrapping/unwrapping bound to file/context/policy and a stable DEK commitment, with no plaintext key persistence.
- [x] 3.3 Derive naming and per-object content keys from the file DEK only; derive wrapping keys from the KEK using separate purposes.
- [x] 3.4 Implement operation-scoped committed FileCryptoContext resolution with redaction, zeroization and typed missing/wrong-master/context failures.
- [x] 3.5 Implement same-DEK rewrap helper and verify that new wrapped bytes preserve key commitment and policy before state CAS.

## 4. Compression and object representation

- [x] 4.1 Implement the fixed safe LZ4 block profile, exact output/whole-input bounds, deterministic 64-byte fallback and supported writer-target vectors.
- [x] 4.2 Integrate standard deterministic AES-SIV and v3 header/body/provenance framing with independent primitive/wrapper/object vectors.
- [x] 4.3 Preserve complete stored-object root/page digests, explicit payload lengths, authenticated policy/context checks and v1/v2 rejection.

## 5. Storage preparation and limits

- [x] 5.1 Route expected/actual payload, page, root and readback paths through one deterministic DEK-bound representation pipeline.
- [x] 5.2 Integrate Raw and paged BlockSplit reads/create/write/truncate while preserving lazy traversal, sparse/no-op reuse, EOF checks and child-before-parent durability.
- [x] 5.3 Account crypto/codec/wrapper/copy scratch and exact v3 overhead with existing map/layout budgets; reject known preparation limits before PUTs.

## 6. Filesystem orchestration

- [x] 6.1 Generate candidate metadata for a caller-assembled declarative empty-file create and include it in the same inode/context/namespace/open/pin/result transaction; do not require completion of the unfinished semantic engine.
- [x] 6.2 Resolve ledger replay/commit ambiguity and reload the authoritative winning context before any encrypted S3 preparation; discard losing candidates.
- [x] 6.3 Read inode/context consistently for first and subsequent content operations, ignore changed defaults, and revalidate context at publication.
- [x] 6.4 Add explicit bounded administrative rewrap orchestration with old/new KEKs, permission/fence/revision checks and exact result replay.

## 7. PostgreSQL persistence

- [x] 7.1 Add the normalized content-metadata entity/table and regular-inode bindings without an inode-delete cascade; update the current code-first schema/catalog/checksum directly.
- [x] 7.2 Extend row/key codecs, bounded point/scan projections, retained-byte estimates and change-event key mappings for opaque context records.
- [x] 7.3 Implement canonical locking, atomic creation/publication/rewrap, named constraint/error classification and privilege validation without crypto/S3 logic.
- [x] 7.4 Update schema/codec/conformance fixtures and scoped test cleanup order for retained context rows; add no compatibility migration.
- [x] 7.5 Test independently connected clients for durable winner reload, key/context binding, revision/fence conflicts, rewrap and post-retirement retention.

## 8. Failure, security and resource evidence

- [x] 8.1 Run both methods across plain, compression-only, encryption-only and combined policies with all existing paging/EOF/no-op boundaries.
- [x] 8.2 Test distinct random candidates under duplicate/concurrent creates, replay after inode retirement, changed defaults before first write, losing-key suppression and crashes around context commit.
- [x] 8.3 Inject failures at wrapping, S3 payload/page/root upload and metadata publication; prove old-or-new content with recoverable keys.
- [x] 8.4 Test wrong masters, swapped wrappers/contexts, commitment/policy mismatch, authenticated out-of-EOF pages, malformed compressed input and reused-buffer leakage.
- [x] 8.5 Test rewrap preserving S3 bytes/names/ContentRef and the DEK, including ambiguity, stale authority and concurrent content publication.
- [x] 8.6 Measure bounded metadata scans, operation-scoped key material and representation/map scratch on large file/map sets; retain no global file-key cache.

## 9. Live integration and delivery

- [x] 9.1 Extend storage/S3 conformance to receive explicitly selected committed file contexts and public test KEKs through the existing provider guards.
- [x] 9.2 Add a bounded PostgreSQL + SeaweedFS composition test for generated-key persistence/reopen/rewrap, with its own private post-probe wrapper in test/tests/support/content_target.rs; introduce no production qualification bypass.
- [x] 9.3 Wire required features/tests in S3 Cargo, test/Cargo.toml, lockfiles, test/run-integration.sh and CI; preserve AWS opt-in and scoped teardown.

## 10. Documentation and final checks

- [x] 10.1 Update ownership, key bootstrap/retention/rewrap, caller KEK/entropy, state/schema changes, encrypted-visible data and current-format reset documentation.
- [x] 10.2 Run feature-matrix and workspace formatting/tests/Clippy/rustdoc, generic state conformance, PostgreSQL adapter tests and dependency/license/audit gates.
- [x] 10.3 Run bounded live integration, verify teardown, and record actual resource/security evidence and unexecuted external checks before completion.

## Notes

This is a draft checklist; research and document revision do not complete
implementation tasks. The latest direction permits state types/tests and SQL
schema changes for bounded opaque file-context metadata, superseding the older
no-state-edit constraint. Crypto and layout execution remain in storage and
orchestration. Block-map pages remain in S3.

One external master wraps generated per-file DEKs. Use only the durable winning
context after creation/replay; retain contexts after inode retirement. Explicit
rewrap preserves the DEK and S3 content; actual DEK rotation and key GC remain
separate work.
