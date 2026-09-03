# Implementation Blueprint: Add Pluggable File Storage Methods

## Design Approach

Create a new `w9pt-storage` crate implementing a backend-neutral file-content repository. Keep the existing `w9pt` protocol/session crate unchanged. The repository is generic over a runtime-neutral target-store contract and dispatches internally between `Raw` and `BlockSplit` methods persisted in each file manifest.

Use immutable payloads and manifests. Separate content preparation from authoritative publication, then provide a small object-backed file-head publisher for standalone operation and tests. Require durable data objects before manifest creation and a conditional head update before a prepared version becomes current.

Version 1 uses identity-encoded, unencrypted payloads, BLAKE3-256 plaintext digests, one object per raw file or nonzero block, and a bounded flat sparse block manifest.

## Files to Create or Modify

```text
Cargo.toml                                      # add workspace member
Cargo.lock                                     # resolved compatible dependencies
README.md                                      # document crate/status

crates/w9pt-storage/
  Cargo.toml
  src/
    lib.rs                                     # public API and crate contract
    config.rs                                  # method default and representation config
    error.rs                                   # typed storage failures
    ids.rs                                     # FileId, MutationId, ObjectKey/version/ref
    limits.rs                                  # validated resource bounds
    object_store.rs                            # runtime-neutral target contract
    publisher.rs                               # object-backed file-head CAS
    repository.rs                              # shared dispatch and preparation
    format/
      mod.rs
      envelope.rs                              # checked object envelope
      head.rs                                  # mutable-head payload codec
      manifest.rs                              # immutable manifest/blob-ref codec
    layout/
      mod.rs                                   # method dispatch
      range.rs                                 # checked logical block spans
      raw.rs                                   # whole-file method
      block_split.rs                           # fixed 32 KiB sparse method
    testing/
      mod.rs
      memory_store.rs                          # deterministic target + failure injection
      conformance.rs                           # reusable target/method assertions
  tests/
    format_golden.rs
    range_planning.rs
    raw_model.rs
    block_split_model.rs
    publication.rs
    crash_recovery.rs
    object_store_conformance.rs
```

Testing helpers may remain crate-private or behind an explicitly development-only feature until a public conformance API is justified.

## Implementation Phases

### Phase 1: Crate and contracts

- Add workspace/package metadata using the existing Rust 2024, Apache-2.0, documentation, `unsafe_code`, and Clippy conventions.
- Define strong identifiers, storage methods, representation identifiers, validated limits, and typed errors.
- Define the generic target contract and document the exact durability/CAS guarantees required from an adapter.
- Build a deterministic memory target before implementing a layout.

### Phase 2: Persistent format

- Implement checked primitive readers/writers and the object envelope.
- Implement canonical head, manifest, blob-reference, raw-layout, and sorted sparse block-map codecs.
- Fix the version-1 method parameters, hash algorithm, codec, cipher, and maximum decoding rules.
- Add independent golden fixtures and malformed input coverage before repository behavior depends on the codec.

### Phase 3: Repository preparation and publication

- Implement key construction, target object verification, immutable creation, manifest preparation, and `ContentRef` validation.
- Implement object-backed file-head create/load/CAS.
- Resolve ambiguous publication through readback and implement bounded rebase attempts.
- Trace target operations in tests to prove data-before-manifest-before-head order.

### Phase 4: Raw method

- Implement bounded full-object reads and digest verification.
- Implement positioned write by checked materialization, zero-gap extension, patching, immutable upload, and manifest preparation.
- Implement shrink, extension, zero-size representation, unchanged-content reuse, and limit rejection.
- Compare all behavior against a byte-vector model.

### Phase 5: Block-split method

- Implement checked range planning and EOF clamping.
- Implement sparse reads and canonical block verification.
- Implement full-block fast writes, partial-block read-modify-write, all-zero omission, and digest-based reuse.
- Implement sparse extension and shrink-tail zeroing.
- Compare randomized operation traces against a byte-vector model.

### Phase 6: Failure, concurrency, and handoff

- Inject deterministic failures around every immutable put and publication step.
- Schedule disjoint and overlapping publisher conflicts and verify rebase/terminal behavior.
- Reopen content from target objects after simulated process loss.
- Document capability boundaries and how a future filesystem transaction replaces `ObjectHeadPublisher`.
- Run the full workspace test, format, Clippy, and dependency-policy checks.

## Testing Strategy

### Format tests

- Independent bytes for empty raw, non-empty raw, sparse block, head, and maximum-value fixtures.
- Unsupported major/minor behavior, unknown tags, truncated headers/payloads, trailing bytes, duplicate/out-of-order blocks, oversized lengths, invalid 32 KiB relationships, and digest corruption.

### Range and layout tests

- Empty operations, exact boundaries, one-byte crossings, multi-block ranges, near-`u64::MAX` overflow, full overwrite, partial overwrite, writes past EOF, all-zero transitions, sparse reads, and truncate shrink/extend cycles.
- Verify padded tail bytes are never observable and shrink-then-extend never resurrects prior data.

### Model/property tests

- Execute generated create/write/read/truncate traces against `Raw`, `BlockSplit`, and a reference `Vec<u8>` model within configured bounds.
- Assert identical logical bytes and sizes after every successful publication.
- Record target operations to assert unchanged data causes no new payload put.

### Publication and crash tests

- Fail before/after payload creation, manifest creation, CAS attempt, and ambiguous CAS completion.
- Reopen after every failure and require old or complete new content.
- Run deterministic writer schedules for disjoint and overlapping byte ranges, bounded conflicts, and retry exhaustion.

### Target conformance

- Atomic `put_if_absent`, opaque version changes, CAS success/conflict, exact range behavior, read-after-publication, and definitive error handling.
- The memory target supplies the reference behavior; later S3/local adapters run the same suite.

## Completion Gate

The change is complete only when both methods pass the same logical content conformance suite, the target is the sole authority after reopen, publication ordering is proven under injected failures, all limits are enforced before amplification, and `w9pt` remains storage-layout neutral.

