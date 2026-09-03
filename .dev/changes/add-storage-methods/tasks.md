# Tasks: Add Pluggable File Storage Methods

## Progress: [37/37]

### 1. Foundations

- [x] 1.1 Add `crates/w9pt-storage` to the workspace with Rust 2024 package metadata, Apache-2.0 licensing, documentation lints, `unsafe_code = "forbid"`, and workspace-compatible Clippy policy.
- [x] 1.2 Define `FileId`, caller-supplied `MutationId`, `ObjectKey`, opaque `ObjectVersion`, `Digest`, `ContentRef`, and `PreparedContent` strong types.
- [x] 1.3 Define `StorageMethod::{Raw, BlockSplit}`, version-1 representation identifiers, persisted 32 KiB block parameters, and validated creation defaults.
- [x] 1.4 Define checked `StorageLimits` and typed configuration, range, format, corruption, target, conflict, ambiguity, and limit errors.

### 2. Target abstraction

- [x] 2.1 Define the generic runtime-neutral target contract for exact get/range-get, atomic immutable put-if-absent, and opaque-version compare-and-swap, including required durability and consistency semantics.
- [x] 2.2 Implement a deterministic in-memory target with version tokens, target-operation tracing, conditional behavior, and before/after failure injection.
- [x] 2.3 Add reusable target conformance tests for immutable creation, exact reads/ranges, CAS creation/replacement/conflict, read-after-publication, and ambiguous-result readback.

### 3. Persistent format

- [x] 3.1 Implement checked little-endian primitive readers/writers and the versioned object envelope with kind, lengths, and checksum validation.
- [x] 3.2 Implement canonical file-head, file-manifest, raw-layout, sorted sparse block-map, blob-reference, hash, codec, and cipher encoding/decoding.
- [x] 3.3 Enforce file identity, generation, logical-size, block-size, sorted/unique index, canonical hole, stored-length, known-tag, decode-limit, and arithmetic invariants.
- [x] 3.4 Add independent golden fixtures plus truncated, malformed, non-canonical, unsupported-version, overflow, trailing-data, and corruption tests.

### 4. Shared repository and publication

- [x] 4.1 Implement checked logical-range/EOF planning and block-span iteration without empty-range underflow or `u64` overflow.
- [x] 4.2 Implement private-prefix key construction using fixed-width file, mutation, attempt, and block identifiers without visible path names.
- [x] 4.3 Implement immutable payload verification/creation and manifest preparation with data-before-manifest ordering and `ContentRef` validation.
- [x] 4.4 Implement object-backed file-head create/load/CAS, ambiguous-result readback, bounded conflict rebasing, and typed retry exhaustion.

### 5. Raw method

- [x] 5.1 Implement empty and non-empty raw creation/read with complete-object verification, exact logical EOF behavior, and raw-materialization bounds.
- [x] 5.2 Implement positioned raw writes with checked zero-gap extension, byte patching, whole-object immutable replacement, and unchanged-content reuse.
- [x] 5.3 Implement raw shrink, sparse-zero extension by materialization, unchanged-size handling, and truncate-to-zero without a payload object.
- [x] 5.4 Add deterministic byte-vector model tests, limit tests, corruption tests, and failure tests for raw behavior.

### 6. Block-split method

- [x] 6.1 Implement 32 KiB block reads with sparse zero synthesis, canonical payload verification, ordered assembly, and EOF slicing.
- [x] 6.2 Implement full-block overwrite without old reads and partial-block read-modify-write from verified data or sparse zeroes.
- [x] 6.3 Implement BLAKE3 unchanged-block reuse, all-zero omission, final-block zero padding, sparse gaps, and writes extending EOF.
- [x] 6.4 Implement shrink-tail zeroing, removal of blocks beyond EOF, truncate-to-zero, sparse extension, and checked maximum-manifest enforcement.
- [x] 6.5 Add boundary, randomized byte-model, corruption, no-op upload, sparse-file, truncate/re-extend, and overflow tests.

### 7. Atomicity, handoff, and completion

- [x] 7.1 Test deterministic disjoint and overlapping writers, CAS conflict rebasing, retry exhaustion, and absence of silent lost updates.
- [x] 7.2 Inject failure after every payload, manifest, and head-publication stage; reopen from target-only state and prove that only the old or complete new version is visible.
- [x] 7.3 Document write-through durability, content-only sync behavior, representation/layout separation, capability limits, future filesystem publication, and deferred packing/encryption/GC; run workspace tests, format, Clippy, and dependency checks.

### 8. Code-review remediation

- [x] 8.1 Add checked public `ContentRef` reconstruction for authoritative metadata adapters and restart tests.
- [x] 8.2 Bind immutable preparation keys to mutation ID, base content identity, and a deterministic logical-operation fingerprint rather than a process-local attempt alone.
- [x] 8.3 Carry preparation identity through `PreparedContent` and reject publication under a different mutation, fingerprint, or base.
- [x] 8.4 Treat a differing readback after an ambiguous CAS as unresolved unless non-commit can be proven, preventing blind reapplication.
- [x] 8.5 Enforce repository-prefix and canonical key-schema membership for loaded manifests and payload references.
- [x] 8.6 Add a caller-enforced maximum to exact target reads and apply object/manifest bounds before transfer or allocation.
- [x] 8.7 Validate retained partial-block padding before shrink or extension can make previously invalid bytes authoritative.
- [x] 8.8 Make head/manifest encoders enforce key bounds symmetrically with decoders.
- [x] 8.9 Precompute checked manifest encoded length before allocating its output buffer.
- [x] 8.10 Add two-run deterministic publication replay tests for both layouts; rerun all workspace validation gates.
