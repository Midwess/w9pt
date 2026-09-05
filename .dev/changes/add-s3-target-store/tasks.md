# Tasks: Add S3 Target Store

## Progress: [49/52]

### 1. Repair immutable-put ambiguity in the target contract

- [x] 1.1 Add `PutIfAbsent::Ambiguous` and document that a conditional immutable write may have committed when its response is lost.
- [x] 1.2 Extend storage ambiguity errors so unresolved immutable creation and mutable-head publication remain typed and distinguishable.
- [x] 1.3 Update `MemoryTarget` so injected failures after immutable creation return `Ambiguous`, while before-dispatch failures remain definite errors.
- [x] 1.4 Update `ContentRepository::put_immutable` to resolve ambiguous creation by bounded exact readback before storing a dependent manifest.
- [x] 1.5 Handle ambiguous readback as exact-byte success, different-byte immutable collision, or typed unresolved ambiguity without claiming absence.
- [x] 1.6 Extend backend-neutral target/repository conformance and crash tests for response-lost immutable puts, missing/failed readback, retry, and late unreachable objects.

### 2. SDK feasibility and crate foundation

- [x] 2.1 Add `crates/w9pt-fs-storage-s3` to the workspace with Rust 2024/Rust 1.94.1 metadata, Apache-2.0 licensing, complete public docs, `unsafe_code = "forbid"`, and workspace Clippy policy.
- [x] 2.2 Pin `aws-sdk-s3` exactly at 1.145.0 with default features disabled, `rt-tokio` enabled, and an explicit feature for the default HTTPS client; add no normal `aws-config` dependency.
- [x] 2.3 Prove the pinned SDK exposes conditional PutObject, conditional HEAD/GET, range/metadata access, streaming bodies, request IDs, and per-operation retry override needed by the design.
- [x] 2.4 Commit and audit the complete resolved Smithy/HTTP/TLS dependency graph and pass `cargo +1.94.1 check/test`; stop for an explicit MSRV/dependency decision on an unresolvable advisory or incompatibility.
- [x] 2.5 Define the crate modules for configuration, target operations, ETag versions, ranges, bodies, error classification, testing, and public exports.
- [x] 2.6 Add dependency-policy tests proving `w9pt`, `w9pt-fs`, `w9pt-fs-state`, and `w9pt-fs-storage` do not gain AWS SDK, Tokio, HTTP, TLS, credential, or S3 types.

### 3. Configuration, client ownership, keys, and version tokens

- [x] 3.1 Implement checked `S3TargetConfig` for bucket, expected owner, requester pays, provider profile, ETag/body/range bounds, read retries, resolution attempts, and body timeout/stall policy.
- [x] 3.2 Implement `S3Target::new` over a caller-created `aws_sdk_s3::Client` without loading credentials, region, endpoint, environment, shared config, or a global runtime.
- [x] 3.3 Derive a private write client or per-operation override with SDK mutation retries disabled and prove caller/environment retry settings cannot override it.
- [x] 3.4 Forward repository `ObjectKey` text verbatim as the bucket key, with tests for all accepted safe punctuation and no second prefix/path normalization.
- [x] 3.5 Implement a bounded versioned ETag-to-`ObjectVersion` codec preserving exact quoting/case and rejecting empty, control-containing, oversized, malformed, trailing, or foreign tokens.
- [x] 3.6 Add independent golden fixtures and round-trip/error tests for configuration, key routing, ETag tokens, expected owner, requester-pays, and redacted diagnostics.

### 4. Complete and ranged reads

- [x] 4.1 Implement `HeadObject` metadata loading with proven not-found handling, nonnegative checked content length, required canonical ETag, expected owner, and requester-pays fields.
- [x] 4.2 Implement `TargetStore::get` as HEAD followed by `GetObject If-Match`, rejecting an oversized object before body request/collection and preventing a HEAD/GET version mixture.
- [x] 4.3 Implement hard-bounded incremental ByteStream collection with exact end length and explicit total/stall timeout behavior independent of the SDK operation timeout.
- [x] 4.4 Implement bounded restart of the complete read when `If-Match` detects a version race, without combining bytes or metadata across attempts.
- [x] 4.5 Implement checked `[start,end)` to inclusive HTTP range conversion and safe empty-range presence handling through HEAD.
- [x] 4.6 Validate range status, canonical `Content-Range`, total object size, content length, ETag, and exact body count; reject ignored, clamped, malformed, short, long, overflowing, and `416` responses.
- [x] 4.7 Add mock/body tests proving no oversized body is polled, missing 403 is not `None`, read retries are bounded, empty/EOF ranges are exact, and stream faults never return partial success.

### 5. Immutable conditional creation

- [x] 5.1 Map `put_if_absent` to one single-part `PutObject` with exact content length, qualified transfer integrity, `If-None-Match: *`, and configured owner/requester fields.
- [x] 5.2 Require a valid response ETag for `Created`; treat a successful/maybe-successful request without usable version metadata as ambiguous.
- [x] 5.3 Map single-attempt `412` to `AlreadyExists` after bounded current ETag lookup, without replacing the current bytes.
- [x] 5.4 Classify `409` through bounded current-state observation and classify timeout, dispatch uncertainty, 408/429/5xx, or response parse loss after possible dispatch as `Ambiguous`.
- [x] 5.5 Map definite construction, invalid request, authentication, permission, expected-owner, and unsupported-provider failures to typed redacted adapter errors.
- [x] 5.6 Capture mock HTTP requests and assert exact bucket/key/headers/body/checksum plus exactly one mutation dispatch for every immutable creation attempt.

### 6. Compare-and-swap and failure classification

- [x] 6.1 Map expected absence to retry-disabled `PutObject If-None-Match: *` and expected presence to retry-disabled `PutObject If-Match: <decoded-etag>`.
- [x] 6.2 Reject invalid/foreign/oversized expected versions before request dispatch and never use S3 version ID as a stronger predicate than ETag.
- [x] 6.3 Require a valid successful replacement ETag and return the exact canonical `ObjectVersion` supplied by subsequent GET/HEAD.
- [x] 6.4 Map a single-attempt `412` to definite `Conflict` and obtain the latest optional current ETag through bounded HEAD without treating the observation as a lock.
- [x] 6.5 Handle conditional `409` and present-CAS `404` through current-state classification, distinguishing proven non-commit from unresolved outcome.
- [x] 6.6 Return `CompareExchange::Ambiguous` for any timeout, dispatch/connection uncertainty, 408/429/5xx, or malformed/incomplete success that may follow commit; never transparently repeat the mutation.
- [x] 6.7 Add table-driven mock tests for construction/auth errors, 403, 404, 408, 409, 412, 416, 429, 5xx, timeout, dispatch loss, parse loss, missing ETag, and redacted request IDs across every operation.

### 7. Conformance and provider qualification

- [x] 7.1 Extend reusable target conformance with missing GET/range, empty ranges, independent clients, concurrent immutable writers, concurrent CAS, ambiguity, and bounded result cases that apply to every backend.
- [ ] 7.2 Run target conformance through two separately constructed S3 clients sharing only the bucket/private namespace.
- [ ] 7.3 Run raw and block-split create/reopen/read/write/truncate/publication/crash suites over the S3 adapter and prove persisted formats/keys remain unchanged.
- [x] 7.4 Add deterministic response-lost tests proving immutable readback success/collision/ambiguity and `ObjectHeadPublisher` CAS readback success/conflict/unresolved ambiguity.
- [x] 7.5 Add a pinned local compatibility job that starts and addresses the exact provider artifact, exercises the complete writable behavior probes through two clients, and still returns explicit unsupported-profile configuration until non-emulator durability and fault evidence is recorded.
- [x] 7.6 Add optional additional provider profiles only with recorded exact version, endpoint/addressing, ETag, range, checksum, versioning, consistency, durability, and fault-test evidence.
- [x] 7.7 Add opt-in live Amazon S3 general-purpose bucket tests with unique caller-supplied namespace, two clients, required-mode environment gating, and no automatic bucket provisioning.
- [x] 7.8 Implement test cleanup only for validated test-owned bucket/prefix targets using separately scoped delete permission; never delete or list outside the exact namespace.

### 8. Security, operations, documentation, and validation

- [x] 8.1 Document minimal runtime IAM for prefix-scoped `GetObject`, `PutObject`, and `ListBucket`, plus separate test-cleanup `DeleteObject`; include expected-owner and conditional-write bucket-policy guidance.
- [x] 8.2 Document verified TLS/SigV4, caller-owned credentials/refresh, endpoint/path-style rules, transfer checksum profiles, server-side bucket-default encryption, and SSE-C exclusion.
- [x] 8.3 Document the ban on external prefix mutation, lifecycle expiration/archive transitions, unsafe delete markers, unconditional mutable-head writes, and ETag-as-content-hash assumptions.
- [x] 8.4 Document performance limits: HEAD+GET complete reads, sequential small block requests, no multipart/cache/coalescing/packing, S3 request cost, and the need for a later benchmark/optimization proposal.
- [x] 8.5 Update root/project documentation with the implemented S3 target boundary, AWS general-purpose qualification, compatible-provider proof policy, and caller runtime/deployment responsibilities.
- [x] 8.6 Run current-toolchain and Rust 1.94.1 workspace tests, all features/targets, live-test compilation, rustfmt, Clippy with warnings denied, docs, license/dependency/security checks, and confirm unrelated dirty work remains unchanged.

## Notes

- Task 2.4 audit decision on 2026-09-05: `cargo audit --file Cargo.lock`
  reports RUSTSEC-2026-0009 for `time 0.3.45`. The first patched release,
  `time 0.3.47`, requires Rust 1.88 and therefore cannot satisfy the approved
  Rust 1.85 baseline. The pinned `aws-sdk-s3 1.96.0` graph also includes
  `lru 0.12.5`, which has RUSTSEC-2026-0002 and RUSTSEC-2026-0253 unsoundness
  warnings and no compatible `0.12.x` fix. Per the proposal stop gate, an
  explicit SDK/MSRV or dependency-patch decision was required. The user selected
  an MSRV upgrade; the revised baseline is Rust 1.94.1 with exact
  `aws-sdk-s3 1.145.0`. The replacement graph passes the Rust 1.94.1 build and
  RustSec audit gates recorded in `dependency-audit.md`.
- Tasks 7.2 and 7.3 are implemented by the opt-in `s3_conformance` harness and
  compile successfully, but remain unchecked until a caller-supplied live AWS
  general-purpose test bucket executes them without skip gating.
- Task 8.6 local validation passed on 2026-09-05: exact Rust 1.94.1 and current
  stable workspace tests with all targets/features, workspace Clippy with
  warnings denied, rustfmt check, docs with warnings denied, live-test
  compilation/skip gating, dependency isolation, complete license metadata
  inventory, and `cargo audit` over the final lockfile. The audit-only legacy
  `h2 0.3` test dependency was removed before the final clean scan.
- Code review follow-up for task 7.5 starts the digest-pinned SeaweedFS 4.42
  artifact, verifies its reported identity, provisions only its ephemeral local
  test bucket, and requires its two-client writable behavior probes to pass.
  The exact compatible-provider profile remains explicitly unsupported because
  emulator behavior does not establish production durability and fault evidence.
