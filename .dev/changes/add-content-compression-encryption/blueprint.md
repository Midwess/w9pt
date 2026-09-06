# Architecture Blueprint: add-content-compression-encryption

Revised: 2026-09-06
Based on the current paged/storage/state contracts and the user's per-file
wrapped-key direction.

## Design Approach

Add one generic persistent content-metadata record per managed regular file.
Its opaque policy and wrapped DEK are created atomically with an unpublished
empty inode. Orchestration then reloads the durable winner, unwraps the DEK
using the caller's master KEK and passes an operation-scoped context to storage.

Storage uses the DEK for protected names and per-object content keys. The KEK
is used only for wrapping. Metadata publication checks a nonsecret prepared
context binding and the current record revision. An explicit rewrap changes
only the opaque envelope/revision; keys, policy and all S3 content stay stable.

Keep the bounded LZ4/deterministic-SIV representation plan, exact preflight,
child-before-parent upload order, lazy mapping pages and stored-root digest
semantics. PostgreSQL stores metadata and key envelopes; S3 stores content and
its physical mapping objects.

## Component Design

### Generic state metadata

Extend state record/key/family enums with ContentMetadata, keyed by filesystem
and content FileId. Its bounded value contains owner inode/context IDs,
immutable opaque policy, optional immutable key commitment, optional opaque
wrapped bytes and record revision. Regular inodes carry the stable context ID.

Add exact reads, bounded keyset scans/cursors, byte accounting and change events.
Validate owner/context associations and atomic creation with the inode. Retain
metadata after inode retirement; generic delete/replacement cannot remove or
reassign keys. A dedicated rewrap change updates only the opaque wrapper with
revision/fence/idempotency validation.

Include a composite inode-with-context snapshot query. Enforce filesystem-scoped
uniqueness for file, context and original owner identities, including retained
metadata whose owner has retired.

The state layer compares values and relationships, not cipher tags or DEKs.
Use dummy opaque bytes in its conformance tests. Representation parsing and
cryptographic proof that a rewrap preserves the DEK belong outside state.

### Candidate generation and committed context

Storage-owned helpers accept file/context binding, pinned policy, explicit
secure entropy and an identified caller-owned master key. Candidate generation
returns only opaque metadata, zeroizing candidate plaintext key buffers.
Orchestration commits that metadata with the namespace/inode creation, resolves
replay/ambiguity, and reads the committed winner before requesting unwrap.

An unwrapped FileCryptoContext contains one selected DEK and its nonsecret
binding, with redaction and cleanup. It never loads all file keys. Storage
cannot prove a metadata blob is committed; the orchestration protocol provides
that authority and is tested explicitly.

### Envelope and content cryptography

Use separate derivation purposes for wrapping, DEK commitment, naming and
per-exact-object SIV keys. Wrapping binds the KEK, master ID and file/context/
policy. Content naming and encryption bind only the stable DEK/context and
object identity, excluding wrapper revision/master information.

The same deterministic representation pipeline produces preflight and upload
bytes and validates exact readback. Shared v3 framing protects root/page/payload
bodies and carries their own creation provenance. Decode authenticates before
using metadata or decompressing. Plaintext modes remain supported with explicit
pinned policy and no DEK generation.

### Focused filesystem orchestration

Add only the helpers needed to generate a file context for a caller-assembled declarative empty-file
create transaction supported by the state contract, resolve committed metadata for content operations and drive
explicit administrative rewrap. Keep dependencies in the existing direction:
filesystem orchestration may call state and storage; storage must not call the
state/PostgreSQL adapter.

Random DEK bytes are generated allocation results. Semantic fingerprints cover
actual requested operands; exact replay reloads the originally selected context.
First content preparation uses that context even after defaults change.

### PostgreSQL adapter

Add a normalized content-metadata table, inode context binding, codecs, exact
reads/scans, locking and dedicated transition persistence. Named constraints
validate shapes and bounded values. A retained metadata row is not deleted by
an inode cascade. Add context revision checks to content publication and rewrap.

Update the current code-first schema/checksum and catalog fixtures directly,
plus byte estimates, privileges, SQLSTATE mappings and cleanup ordering. No
crypto/RNG/S3 operation executes in the adapter and no plaintext key is serialized.

## File Blueprint

### Create

| File | Purpose |
| --- | --- |
| Storage `src/representation/mod.rs` | Shared exact representation pipeline |
| Storage `src/representation/compression.rs` | Frozen bounded LZ4 profile |
| Storage `src/representation/encryption.rs` | SIV/KDF/AAD/protected tokens |
| Storage `src/representation/file_keys.rs` | Candidate DEK generation, wrapping, committed unwrap, rewrap, secret contexts |
| Storage `tests/representation.rs`, `tests/protected_objects.rs` | Transform/wrapper/context vectors and adversarial cases |
| State `src/content_metadata.rs` | Generic bounded metadata values and stable context binding |
| PostgreSQL `src/schema/entities/w9pt_fs_state_content_metadata.rs` | Current normalized metadata table entity |
| PostgreSQL `tests/content_metadata.rs` | Durable key selection, replay, retention and rewrap |
| Filesystem `src/content_context.rs` | Focused state/storage orchestration helpers |
| Standalone `test/tests/content_encryption.rs` | Bounded PostgreSQL + SeaweedFS key/content composition |
| Standalone `test/tests/support/content_target.rs` | Private post-probe compatibility wrapper; no production guarantee bypass |

Names under Storage/State/PostgreSQL/Filesystem are relative to their matching
workspace crates. Split internal modules further only when implementation size
warrants it; do not build an algorithm/plugin framework.

### Modify

| Area | Change |
| --- | --- |
| Storage config/IDs/limits/errors/exports | Pinned context, optional LZ4/SIV, entropy/key errors and scratch bounds |
| Storage envelope/root/page/blob codecs and keys | v3 protected provenance and exact lengths; DEK-derived naming |
| Storage repository/publisher and Raw/BlockSplit planning | Explicit selected file context and deterministic two-pass transforms |
| Storage tests/conformance and Cargo feature graph | Representation/wrapper/retry/resource matrix and reviewed optional dependencies |
| State records/read/commit/change/limits/exports | Context family, inode binding, publication validation, dedicated rewrap, retention |
| State memory reference and conformance | Generic opaque metadata semantics and failure tests |
| PostgreSQL current schema and codecs | Table/inode fields, bounded projections, writes, named constraints and checksums |
| PostgreSQL commit/validation/sqlstate/testing | Lock order, binding checks, privileges, cleanup and conformance |
| Filesystem identity/execution/result fixtures | Context allocation/result handling and ledger-first orchestration |
| S3 Cargo/tests, `test/Cargo.toml`, lockfiles | Explicit feature dependencies and cross-layer test setup |
| `test/run-integration.sh`, `.github/workflows/s3-target.yml` | Required bounded matrix and composition wiring |
| README/project/guidance | Per-file key ownership, master rewrap, current schema reset and retention limits |

## State/Storage Handoff

The public ContentRef may keep its bounded root fields. Add a nonsecret stable
context/policy/key binding to PreparedContent and the authoritative publication
validation. The policy binding carries exact bounded format/bytes for generic
state comparison; any cryptographic policy commitment remains storage-owned. Managed file operations supply a checked committed file context to
storage; state compares those binding fields without interpreting representation.

Read inode and its context in a single snapshot. Publication revalidates the
context revision with inode/base/fence conditions. A rewrap can conservatively
conflict with an in-flight writer, which reloads the same DEK from the new wrapper.

Standalone encrypted repositories need durable context metadata supplied by
the caller as well as target objects and the master; an S3 head alone cannot
recover a DEK now stored in state. Test fixtures must represent that dependency
explicitly rather than recreating a global in-memory master/file-key registry.

## Implementation Phases

1. Finalize generic record model, bindings, immutable/rewrap/lifetime transitions,
   current formats, cryptographic profile and dependency gates.
2. Extend state types, memory semantics, lookup/pagination, atomic context/inode
   creation, publication association and rewrap conformance using opaque fixtures.
3. Implement storage candidate/wrap/unwrap/rewrap helpers with explicit entropy,
   key commitment, separated derivation and secret cleanup.
4. Implement the bounded LZ4/SIV representation pipeline and v3 protected objects.
5. Integrate deterministic preparation and current-context publication checks.
6. Add focused orchestration for durable winner selection and bounded rewrap.
7. Implement PostgreSQL schema/codec/query/transaction support and independent
   client tests for context lifetime and rewrap.
8. Run adversarial/resource and real provider/composition tests, then required
   workspace/dependency/doc gates and record actual evidence.

The executable checklist is `tasks.md`.

## Testing Strategy

Generic state conformance must cover bounded opaque records, atomic creation,
wrong owner/context, missing references, stable identity/policy, exact replay,
context revision/fence conflicts, rewrap restrictions and key retention after
inode retirement. These tests do not need to execute cryptography.

Storage tests use public deterministic test entropy and masters for known
vectors. Verify generated candidates differ, wrapped bytes authenticate their
file binding, wrong masters fail, and only selected committed candidates become
usable content contexts. Transform tests preserve all existing paging, EOF,
no-op, full/partial overwrite, truncation and memory contracts.

Cross-layer tests deliberately create different candidates for matching retries
and concurrent creates, drop local state after commit, and reopen from PostgreSQL
using one supplied master. Cover crashes before/after context commit, first S3
upload, parent/root upload and final metadata publication. A losing candidate
must produce no S3 data selected by a winner.

Rewrap tests verify unchanged DEK commitment, policy, ContentRef, S3 names and
ciphertext; include stale revisions/fences, exact replay, ambiguous result and
concurrent content mutation. Old master retirement is an operator-controlled
transition, not an automatic guarantee.

Retain primitive/AAD/token/codec golden vectors, authenticated-context swaps,
malformed compressed/reused buffers, exact retry equality and measured transform
scratch. Use synthetic large maps and many metadata rows without retaining all
plaintext keys. Scans, wrappers and per-operation key contexts stay bounded.

Run the existing representation matrix against the real S3 adapter, using
required SeaweedFS probes/private compatibility wrappers and explicitly supplied
state metadata fixtures. Add one bounded PostgreSQL + SeaweedFS composition test
with public test masters. Keep AWS opt-in, scoped cleanup and qualification
claims unchanged; do not add a server to the production crates.

Run feature-matrix and workspace tests, formatting, Clippy, rustdoc, dependency/
source isolation, license and audit gates under Rust 1.94.1. State and PostgreSQL
may now change for metadata, but no direct cryptographic or runtime logic may
leak across their boundary. Record security/cleanup limitations accurately.

## Spec Integration

Update storage and live-integration deltas, and add filesystem-state-store and
postgres-state-adapter deltas for content metadata, atomic selection, publication,
rewrap and lifetime. Existing authority ownership and S3 mapping layout remain
intact. No implementation is authorized by this draft alone.
