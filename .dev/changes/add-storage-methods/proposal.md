# Add Pluggable File Storage Methods

Status: approved

## Summary

Add a new backend-neutral `w9pt-fs-storage` crate that distributes one logical file's content across a caller-provided target object store. The first persisted storage methods are:

- `raw`: one immutable object containing the complete logical file;
- `block-split`: sparse, file-relative 32 KiB logical blocks stored as immutable objects.

The crate will provide positioned reads, prepared writes, truncation, checked persistent manifests, integrity verification, and an optional object-backed compare-and-swap publisher. It will not add S3 behavior or storage-layout details to the existing `w9pt` protocol/session crate.

In this proposal, `raw` names the whole-file layout. It does not mean an encryption or compression mode. Payload representation is a separate concern; the first implementation stores identity-encoded, unencrypted bytes while persisting explicit representation identifiers for later compatible additions.

## Motivation

The Sans-I/O core is ready to emit backend-neutral positioned file operations, but the workspace has no implementation for turning logical file bytes into durable target objects. Implementing an S3 filesystem directly would mix several independent problems at once: namespace semantics, inode metadata, target SDK integration, content layout, concurrency, and durability.

A content repository establishes the storage method first. It gives future filesystem and S3 crates one tested contract for:

- selecting and persisting a file-content layout;
- handling byte-range reads, writes, holes, EOF, and truncate consistently;
- writing immutable dependencies before publishing metadata;
- detecting unchanged content and stored-data corruption;
- reopening all authoritative state from the target store without an external database;
- testing target-independent behavior before adding an S3 client.

The split also preserves the existing rule that protocol requests contain no object keys, manifests, blocks, or SDK values.

## Goals

- Introduce a separate `w9pt-fs-storage` workspace crate without adding dependencies or storage APIs to `w9pt`.
- Define a runtime-neutral asynchronous target-object contract with immutable creation, reads, ranges, and single-key compare-and-swap publication.
- Persist a self-describing storage method in every file manifest so configuration changes cannot reinterpret existing data.
- Implement the `raw` whole-file method with explicit size bounds and documented rewrite amplification.
- Implement the `block-split` method using sparse, fixed 32 KiB logical plaintext blocks.
- Preserve exact positioned I/O, authoritative logical EOF, zero-filled holes, and safe shrink/extend behavior.
- Hash canonical plaintext content for no-op detection and end-to-end integrity verification.
- Separate immutable content preparation from publication so a future filesystem transaction can commit the new content reference with inode metadata.
- Provide an object-backed file-head publisher for standalone use and deterministic concurrency testing.
- Establish data-before-manifest-before-head ordering and bounded compare-and-swap conflict handling.
- Supply a deterministic in-memory target, conformance suite, model tests, malformed-format tests, and failure injection.

## Scope

### In scope

- Root workspace membership for `crates/w9pt-fs-storage`.
- Strong storage identifiers, validated limits, method configuration, content references, and typed errors.
- A target object-store interface whose opaque version tokens can represent ETags, generation numbers, or local equivalents without exposing a specific provider.
- A checked binary envelope, file head, file manifest, blob reference, and block-entry format.
- Caller-supplied file and mutation identities; no hidden randomness or clock access.
- Identity payload representation and BLAKE3-256 plaintext digests in format version 1.
- `raw` create/read/positioned-write/truncate behavior.
- `block-split` create/read/positioned-write/truncate behavior, including partial-block read-modify-write, full-block fast paths, sparse holes, all-zero omission, and EOF padding.
- Immutable object preparation plus object-backed head load and conditional publication.
- Write-through durability: a successful publication is already durable according to the target contract.
- Explicit limits for raw-file materialization, manifest size, object size, retry count, and caller-supplied data ranges.
- Reusable conformance tests over a deterministic in-memory target.

### Out of scope

- An S3 SDK adapter, credentials, HTTP transport, Tokio runtime, listener, daemon, or deployment configuration.
- Visible paths, directories, inode allocation, ownership, timestamps, links, QIDs, rename, unlink, open-unlinked lifetime, xattrs, and locks.
- Changes to `FilesystemOperation`, `Effect`, or any 9P wire type.
- Compression or authenticated-encryption implementations; format identifiers are persisted but only identity/no-encryption is accepted in version 1.
- Packed segment objects, range coalescing, caching, read-ahead, write-back, or background flushing.
- Content deduplication, content-addressed snapshots, branches, release manifests, or SteamPipe-compatible behavior.
- Paged or tree-backed block manifests for unbounded file sizes.
- Garbage collection, compaction, or immediate deletion of unreachable immutable objects.
- Multi-writer election, leases, distributed locks, or reconnect-spanning filesystem idempotency.
- A full filesystem backend that completes `w9pt::Effect::Filesystem`.

## Acceptance Criteria

- `w9pt-fs-storage` can create, reopen, read, write, and truncate files using either persisted storage method over the in-memory target.
- Reopening uses only target-store objects; no process-local index is authoritative.
- Existing files remain readable after the configured default storage method changes.
- `raw` mutations rewrite and atomically publish one complete file object within configured limits.
- `block-split` reads and writes match a byte-vector reference model across aligned, unaligned, cross-block, sparse, EOF, and truncate cases.
- The decoded form of every materialized block is exactly 32 KiB; padding is never returned beyond logical EOF.
- Unchanged hashes reuse prior references, all-zero blocks remain absent, and read-time digest mismatches return corruption errors.
- Failure injection after each preparation/publication step exposes either the old version or the complete new version, never partial content.
- Concurrent publication conflicts never silently lose an update and terminate within the configured retry bound.
- Independent golden fixtures and malformed inputs exercise format version, length, ordering, overflow, and checksum validation.
- `cargo test --workspace --all-targets`, formatting checks, and Clippy with warnings denied pass.
- The existing `w9pt` public operation/effect contract remains unchanged.

## Risks

- A raw partial write has whole-file download, memory, and upload amplification; strict limits and clear method guidance are required.
- One target object per 32 KiB block has high request and object-count cost. It is an intentionally simple first layout, not the final packed-segment design.
- A flat sparse block manifest eventually becomes too large. Version 1 must enforce a manifest bound rather than allocating unbounded state.
- Partial writes can lose concurrent updates unless publication uses conditional revision checks and rebases or returns a typed conflict.
- A shrink that does not zero the retained tail can reveal stale bytes after later extension.
- Hash equality is suitable for no-op detection but is not a substitute for publication concurrency control.
- Plaintext hashes reveal equality to anyone able to inspect private metadata; encryption and keyed identity require a separate design.
- Compression and encryption added later can break range behavior or persistent interpretation unless representation identifiers remain orthogonal to layout.
- Crashes and failed CAS attempts leave unreachable immutable objects. They are safe but consume space until a later garbage-collection change.
- Target providers differ in conditional-write and consistency guarantees; adapters must fail configuration rather than claim guarantees they cannot provide.
- Prior ZeroFS research is architectural evidence only. No AGPL implementation code, tests, format, or distinctive structure may be copied or translated.

## Dependencies

- Rust 2024 with the workspace's Rust 1.85 baseline.
- A narrowly scoped, license-compatible BLAKE3 implementation in `w9pt-fs-storage`; the dependency-free policy for `w9pt` remains unchanged.
- A host-provided target implementation satisfying durable immutable puts, exact reads, atomic single-key compare-and-swap, and read-after-publication behavior.
- Caller-provided `FileId` and `MutationId` values and any scheduling/runtime used to poll target futures.
- A future filesystem semantic-engine proposal will integrate prepared content references with inode and namespace transactions.
- A future S3-adapter proposal will map the target contract to one selected SDK and provider configuration.

