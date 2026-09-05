# Tasks: Add SeaweedFS Storage Integration Tests

## Progress: [24/24]

### 1. Freeze scope and evidence boundaries

- [x] 1.1 Record the current target-probe, repository-conformance, Compose, CI, and TCP coverage and the exact missing SeaweedFS repository composition.
- [x] 1.2 Fix the exact SeaweedFS image digest/version, required environment names, scenario prefix scheme, payload/request bounds, and readiness limits.
- [x] 1.3 Document that the TCP fixture, restart durability, response-loss ambiguity, multi-node behavior, and production provider qualification remain outside this change.

### 2. Strengthen reusable repository conformance

- [x] 2.1 Refactor repository conformance into bounded method/scenario helpers while preserving the existing combined entry point.
- [x] 2.2 Add the Raw create/reopen/unaligned-write/gap-extension/shrink/re-extension byte-vector lifecycle.
- [x] 2.3 Add BlockSplit multi-block reads, cross-boundary partial write, exact full-block overwrite, sparse-hole, all-zero, shrink, re-extension, and EOF cases.
- [x] 2.4 Add independent-client reopen, immediate visibility, persisted-method/default-change, and exact immutable reuse assertions.
- [x] 2.5 Use checked arithmetic, stable distinct file/mutation IDs, scenario-isolated prefixes, and bounded two-to-four-block payloads.
- [x] 2.6 Run the expanded helper against independent memory targets and retain the writable-guarantee rejection regression.

### 3. Compose the real S3 adapter with the repository

- [x] 3.1 Add a private external-test-only `CompatibilityProbeTarget` that delegates every `TargetStore` method to `S3Target` and has no exported or feature-gated production path.
- [x] 3.2 Build two local AWS SDK clients independently, construct two unqualified targets, and verify both advertise no writable guarantees.
- [x] 3.3 Run `S3Target::probe_pair` successfully before constructing compatibility wrappers.
- [x] 3.4 Execute the full Raw and BlockSplit repository matrix through the wrapped real S3 targets.
- [x] 3.5 Assert both ordinary targets remain unqualified and the exact SeaweedFS compatible profile remains unsupported before and after the matrix.

### 4. Add deterministic publication-boundary evidence

- [x] 4.1 For both methods, discard a prepared-but-unpublished version and prove an independent client still observes the complete old version.
- [x] 4.2 For both methods, discard the successful publication result and prove an independent client observes the complete new version.
- [x] 4.3 For both methods, control two competing preparations, reject the stale CAS, reread/reprepare, and prove one complete serial byte result.
- [x] 4.4 Keep timeout, malformed-response, transport-loss, and ambiguous-commit fault injection in the deterministic SDK replay suite rather than Compose timing tests.

### 5. Automate and document

- [x] 5.1 Add an explicit required SeaweedFS repository-test command to `test/run-integration.sh` after provider identity and bucket setup.
- [x] 5.2 Add the explicit required repository step to `.github/workflows/s3-target.yml`, with bounded readiness, logs on failure, isolated prefixes, and unconditional scoped teardown.
- [x] 5.3 Update test, S3 adapter, root, and project documentation with the new behavioral evidence and unchanged production non-support.

### 6. Validate

- [x] 6.1 Run memory/model, offline S3, required SeaweedFS, optional-live-test compilation/skip, PostgreSQL, and TCP integration suites with locked dependencies.
- [x] 6.2 Run Rust 1.94.1 workspace tests, rustfmt, Clippy with warnings denied, rustdoc with warnings denied, dependency isolation, license metadata, and RustSec audit.
- [x] 6.3 Record exact SeaweedFS 4.42 results, verify Compose removed its containers/network/volumes, and reconcile all task/spec checkboxes with evidence.

## Notes

- Required integration passed on 2026-09-06 against
  `chrislusf/seaweedfs@sha256:f7cbc8bdbbf60a1aaba7d61784a3bdff3ec1e0657f6ad0b26d5b6ab2cd9d0dc6`;
  the runner verified `Server: SeaweedFS 30GB 4.42` before storage work.
- The exact target probe passed, followed by
  `live_pinned_compatible_provider_repository_matrix_remains_unqualified`
  passing the expanded Raw, four-logical-block BlockSplit, independent reopen,
  immutable reuse, publication-boundary, and stale-CAS/reprepare matrix through
  two independently constructed clients and the private post-probe wrapper.
- The same required Compose run passed PostgreSQL migration/state conformance
  and the TCP 9P-to-SeaweedFS smoke test. Project
  `w9pt-integration-3662097` then had zero labeled containers, networks, or
  volumes; `test/run-integration.sh` remains executable mode 0755.
- Rust 1.94.1 root and standalone `test/` formatting, tests, Clippy with warnings
  denied, rustdoc with warnings denied, dependency isolation, and license
  metadata checks passed. `test/Cargo.lock` audits cleanly. The root audit passes
  with RUSTSEC-2026-0235 and RUSTSEC-2023-0071 ignored only after `cargo tree`
  proves their `rkyv 0.7.46` and `rsa 0.9.10` packages unreachable under every
  workspace feature and target; CI enforces the same reachability guard.
- All 24 tasks are complete and map to the delta's five requirements and twenty
  scenarios. SeaweedFS remains unqualified compatibility evidence only; no
  restart, transport-ambiguity, multi-node, TLS/lifecycle, or production
  durability claim was added.
