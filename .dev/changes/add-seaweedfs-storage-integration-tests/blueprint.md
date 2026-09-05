# Implementation Blueprint: SeaweedFS Storage Integration Tests

## Design Approach

Add one required live compatibility layer between the existing target probes and
the application-level TCP smoke test:

```text
digest-pinned SeaweedFS 4.42
  -> two independently built AWS SDK clients
  -> two unqualified S3Target candidates
  -> target and pair behavior probes
  -> private integration-test-only guarantee wrapper
  -> ContentRepository Raw and BlockSplit matrices
  -> assert production qualification remains closed
```

The wrapper is evidence plumbing, not a provider profile. It exists only in an
external integration-test crate/module and delegates the real target operations.
No production constructor, feature, or public type may expose it.

## Test-Only Target Wrapper

The integration test should use a locally owned newtype so Rust permits a
`TargetStore` implementation without modifying either production crate:

```rust
struct CompatibilityProbeTarget {
    inner: S3Target,
}

impl TargetStore for CompatibilityProbeTarget {
    type Error = S3Error;

    fn guarantees(&self) -> TargetGuarantees {
        TargetGuarantees::REQUIRED
    }

    // Delegate get, get_range, put_if_absent, and compare_exchange exactly.
}
```

The field and constructor stay private. A single helper owns the only intended
construction path: it accepts two unqualified candidates, verifies both report
no guarantees, runs `S3Target::probe_pair`, and returns two wrappers. Tests must
retain separate clones of the original candidates so they can prove the
production state remains unqualified after repository operations.

Do not add an unchecked repository constructor. Do not make `qualified` mutable
outside `S3Target::qualify_pair`. Do not add a `test-support` feature to the S3
adapter for this purpose.

## Reusable Repository Conformance

Refactor the existing combined helper only as needed to keep the matrix readable:

- preserve `check_repository_conformance(first, second, namespace)` for callers
  that want both methods;
- optionally add a method-scoped internal/public-testing helper taking explicit
  method, file IDs, mutation IDs, and a scenario suffix;
- validate target guarantees through normal `ContentRepository::new` calls;
- keep all inputs deterministic, checked, and bounded;
- use different repository prefixes for Raw, BlockSplit, publication-boundary,
  and concurrency scenarios.

The richer helper must continue to pass against independent `MemoryTarget`
clients. Live AWS conformance automatically receives the stronger matrix because
it already invokes the combined repository helper after real qualification.

## Raw Matrix

For one bounded byte-vector model:

1. Create and publish empty/nonempty content.
2. Reopen it through the second target client.
3. Apply an unaligned positioned overwrite and publish.
4. Write beyond EOF and verify the logical gap is zero-filled.
5. Shrink to a position inside existing data and publish.
6. Re-extend and verify discarded bytes never return.
7. Reopen after each publication and compare exact bytes and EOF.

## BlockSplit Matrix

Use `BLOCK_SIZE_V1` and checked arithmetic to build a two-to-four-block model:

1. Create content crossing at least two block boundaries.
2. Read exact ranges wholly within blocks, across boundaries, and through EOF.
3. Partially overwrite a range spanning two adjacent blocks.
4. Replace one full aligned block.
5. Write beyond at least one absent block and verify the intervening range reads
   as zeros without a materialized hole.
6. Replace a materialized block with all zeros and verify content reads as a
   hole.
7. Shrink within the final retained block, publish, re-extend, and verify the
   discarded tail is zero.
8. Reconstruct/reopen the final `ContentRef` through the independent client with
   Raw configured as its new-file default and verify persisted BlockSplit
   dispatch remains authoritative.

Avoid large randomized network traces. The existing memory model remains the
exhaustive operation generator; live Compose tests cover representative
composition boundaries.

## Publication and Concurrency Matrix

For both methods:

- Prepare a new immutable version but do not publish its head. Drop all local
  preparation state and prove the second client still loads the old head and old
  bytes.
- Publish another version, discard its returned `PublishedContent`, and prove the
  second client loads the complete new head and bytes.
- Prepare two distinct updates from the same head. Publish candidate A, require
  candidate B's stale CAS to return conflict, reload A, reprepare B's semantic
  write against A, publish it, and compare against the byte-vector serial model.
- Reuse an identical immutable preparation from the second client and verify the
  repository accepts only exact existing bytes.

Standalone `ObjectHeadPublisher` is used only to make these test transitions
observable. This does not replace authoritative inode publication in clustered
filesystem operation.

## Provider Guard Assertions

The SeaweedFS test must assert all of the following before and after the matrix:

- each ordinary `S3Target` candidate reports `is_qualified() == false`;
- each candidate advertises `TargetGuarantees::NONE`;
- `S3ProviderProfile::qualified_compatible("SeaweedFS", "4.42")` returns
  `UnsupportedProviderProfile`;
- the Amazon-only `qualify_pair` path is not called.

Passing the suite records behavioral compatibility for the exact digest only. It
does not prove durable acknowledgement, TLS/SigV4 deployment, lifecycle safety,
restart recovery, multi-node consistency, or production support.

## Automation

### Local runner

`test/run-integration.sh` should:

1. Start the exact Compose project and establish trap-based cleanup.
2. Bound SeaweedFS readiness and verify `Server: SeaweedFS 30GB 4.42`.
3. Create only the ephemeral test bucket.
4. Run the existing target probe in required mode.
5. Run the new repository compatibility test in required mode with a distinct
   child prefix.
6. Continue to PostgreSQL and TCP smoke tests.
7. Print provider logs on any integration failure and always remove volumes and
   orphans.

### CI

The storage-integration job should make the repository test an explicit named
step rather than relying on a broad test command whose environment-gated body
could return early. Required-mode variables must be set at the job level. Keep
the exact image digest, provider identity check, unique CI namespace, and final
Compose cleanup.

## Files to Modify

```text
crates/w9pt-fs-storage/
  src/testing/repository_conformance.rs
  tests/object_store_conformance.rs

crates/w9pt-fs-storage-s3/
  tests/s3_conformance.rs
  README.md

test/
  run-integration.sh
  README.md

.github/workflows/s3-target.yml
README.md
.dev/project.md
```

`test/compose.yaml` should remain unchanged unless a concrete readiness defect is
found during implementation. The TCP fixture and production S3 adapter source
are intentionally unchanged.

## Implementation Phases

### Phase 1: Strengthen backend-neutral repository checks

Refactor the current helper, add bounded method-specific models, and verify them
against independent memory targets.

### Phase 2: Add the SeaweedFS repository bridge

Implement the private delegating wrapper, independently build both SDK clients,
run target probes, and execute the Raw/BlockSplit matrix.

### Phase 3: Add publication-boundary evidence

Implement prepared-but-unpublished, published-result-discarded, immutable reuse,
and controlled stale-CAS scenarios for both methods.

### Phase 4: Enforce provider non-qualification

Assert the unqualified state and unsupported compatible profile before and after
all repository work.

### Phase 5: Wire Compose and CI

Add explicit required-mode commands, bounded failure diagnostics, isolated
prefixes, and teardown checks.

### Phase 6: Document and validate

Update evidence/non-claims and run the complete locked quality and integration
gates.

## Testing Strategy

- Deterministic memory-target regression for every expanded reusable scenario.
- Offline S3 request/replay suite unchanged and passing.
- Required digest-pinned SeaweedFS target and repository tests with two clients.
- Existing optional live AWS test receives the expanded repository matrix.
- Existing TCP and PostgreSQL integration smoke/conformance tests remain passing.
- Rust 1.94.1 workspace tests, S3 all-features tests, rustfmt, Clippy with
  warnings denied, rustdoc with warnings denied, dependency isolation, license
  metadata, and RustSec audit.

## Stop Conditions

Return the proposal to design review if implementation would require:

- changing SeaweedFS from unsupported to a production profile;
- weakening `ContentRepository` guarantee validation;
- exporting the compatibility wrapper or adding an unchecked public API;
- modifying persistent storage formats or key identities;
- treating timing-based Compose interruption as proof of ambiguous-write or
  durability behavior;
- adding AWS SDK, Tokio, Docker, or SeaweedFS dependencies to a core crate.
