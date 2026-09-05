# Add SeaweedFS Storage Integration Tests

Status: approved

## Summary

Expand the digest-pinned SeaweedFS 4.42 integration suite so it exercises the
real `w9pt-fs-storage-s3::S3Target` together with
`w9pt-fs-storage::ContentRepository` for both `Raw` and `BlockSplit` content.
The suite will use two independently constructed S3 clients, run the existing
target behavior probes first, and then execute bounded repository lifecycle,
publication-boundary, and controlled-concurrency scenarios against the live
Compose service.

SeaweedFS remains compatibility evidence only. The production adapter will
continue to advertise no writable guarantees for custom endpoints, and the
SeaweedFS compatible-provider profile will remain explicitly unsupported.

## Motivation

The current SeaweedFS job proves exact S3 target operations and conditional
write behavior, while the TCP fixture separately proves 9P effect forwarding
through direct whole-object SDK calls. Neither path constructs a
`ContentRepository`, so no current live SeaweedFS test covers the Raw or sparse
32 KiB BlockSplit layouts. BlockSplit has strong deterministic memory/model
coverage, but its multi-object manifests, cross-block reads and writes, sparse
holes, and truncate behavior have not been composed with the real S3 adapter
against SeaweedFS.

This change closes that evidence gap without treating an emulator pass as proof
of production durability or broad S3 compatibility.

## Scope

### In scope

- Keep SeaweedFS 4.42 pinned by exact image digest and verify its reported
  server identity before exercising it.
- Construct two AWS SDK S3 clients independently and two unqualified
  `S3Target` candidates over one ephemeral test bucket.
- Run `S3Target::probe_pair` before repository operations.
- Add a private integration-test-only `TargetStore` delegator that permits
  `ContentRepository` construction only inside the compatibility test after the
  pair probe succeeds.
- Exercise Raw create, reopen, positioned write, gap extension, truncate,
  re-extension, publication, and independent-client visibility.
- Exercise BlockSplit multi-block creation, boundary reads, cross-block partial
  writes, exact full-block overwrite, sparse holes, all-zero omission, shrink,
  re-extension, and exact EOF behavior.
- Exercise deterministic prepared-but-unpublished, published-result-discarded,
  and stale-publication conflict boundaries without timing-dependent container
  failure injection.
- Strengthen reusable repository conformance where the scenarios apply equally
  to memory targets, live AWS S3, and the test-only SeaweedFS wrapper.
- Make the Compose runner and CI execute the new suite in required mode with
  bounded readiness, isolated prefixes, failure logs, and guaranteed teardown.
- Document exactly what the new evidence proves and does not prove.

### Out of scope

- Adding SeaweedFS to `S3ProviderProfile` or claiming production support.
- Changing `S3Target::qualify_pair`, endpoint validation, qualification state,
  or advertised target guarantees.
- Adding an unchecked `ContentRepository` constructor or a production/test
  feature that can bypass target guarantees.
- Changing Raw, BlockSplit, manifest, key, digest, or publication formats.
- Timing-sensitive container kills, network partition simulation, restart
  durability claims, or multi-node SeaweedFS qualification.
- Replacing deterministic SDK replay tests for timeout, response-loss,
  malformed-response, or ambiguous-commit classification.
- Refactoring the TCP fixture, integrating the unfinished filesystem semantic
  engine, or claiming authoritative PostgreSQL/S3 publication.
- Broad object listing or deletion outside the ephemeral Compose project.

## Success Criteria

- The required SeaweedFS job executes target probes and the complete Raw and
  BlockSplit repository matrix without skip gating.
- An independent repository client can reopen every published content version
  and observe exactly the expected bytes and logical EOF.
- Controlled competing publications expose one complete serial order and never
  mixed content.
- Preparation loss exposes the old published content; loss of the publication
  return value still exposes the complete published content to another client.
- SeaweedFS targets remain unqualified before and after the suite, advertise
  `TargetGuarantees::NONE`, and the compatible-profile constructor still
  returns `UnsupportedProviderProfile`.
- Existing memory/model, offline S3, live-AWS opt-in, TCP smoke, workspace lint,
  documentation, dependency-isolation, and audit gates remain green.

## Risks

| Risk | Mitigation |
| --- | --- |
| A test wrapper is mistaken for production qualification | Keep it private to the external integration test, construct it only after `probe_pair`, use compatibility-only naming, and assert all production qualification guards before and after the repository suite. |
| Small happy-path data fails to exercise BlockSplit | Use deterministic two-to-four-block traces covering aligned, unaligned, sparse, zero, shrink, and re-extension boundaries. |
| Parallel tests collide in one bucket | Give every method/scenario a distinct child prefix and stable non-overlapping file and mutation IDs. |
| Emulator startup or CI becomes flaky | Bound readiness attempts and test work, verify exact identity, print provider logs on failure, and always tear down the unique Compose project. |
| Container interruption is misread as response-loss evidence | Model only deterministic API cut points in Compose and leave transport ambiguity to existing captured-SDK tests. |
| Standalone object heads are mistaken for clustered authority | Label head publication as test-only standalone behavior; PostgreSQL inode transactions remain the sole clustered publisher. |

## Dependencies

- Existing `w9pt-fs-storage` Raw/BlockSplit repository and reusable conformance
  helpers.
- Existing `w9pt-fs-storage-s3` target probes, caller-owned AWS SDK clients, and
  fail-closed compatible-provider policy.
- Digest-pinned `chrislusf/seaweedfs` 4.42 service in `test/compose.yaml`.
- Docker Compose in local integration runs and GitHub Actions.
- No new production dependency or persistent-format version is required.
