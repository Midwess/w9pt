# Add S3 Target Store

Status: approved

## Summary

Add `w9pt-fs-storage-s3`, a production-oriented adapter that implements
`w9pt_fs_storage::TargetStore` over a caller-created AWS SDK for Rust S3 client.
The adapter maps exact bounded reads, exact range reads, immutable
create-if-absent, and opaque-version compare-and-swap onto S3 `HEAD`, `GET`, and
conditional `PutObject` requests.

The supported production baseline is an Amazon S3 general-purpose bucket.
Another S3-compatible provider/version/configuration is supported only after it
passes the full target conformance and failure suite; accepting S3-shaped HTTP
requests is not sufficient evidence of durability, consistency, or conditional
write semantics.

The host continues to own region, endpoint, credentials, TLS/HTTP configuration,
timeouts, Tokio runtime, bucket provisioning, IAM/bucket policy, and deployment.
The adapter owns checked request mapping, ETag version tokens, bounded response
collection, retry suppression for mutations, failure classification, and honest
target guarantees.

## Motivation

`w9pt-fs-storage` already implements raw and sparse block-split content over a
runtime-neutral `TargetStore`, but its only concrete implementation is the
deterministic memory target. A real server still needs a target adapter that can
store immutable payloads/manifests and, for standalone publication and
conformance, conditionally replace one mutable object head.

A thin, ordinary S3 client wrapper is not enough. The storage contract requires:

- successful writes to be durable before acknowledgment;
- atomic `If-None-Match: *` creation;
- atomic `If-Match` replacement against an opaque version;
- immediate read-after-write visibility;
- exact rather than clamped ranges;
- rejection of oversized objects before body collection;
- explicit ambiguity when a conditional request may have committed.

Amazon S3 documents strong read-after-write consistency and atomic single-key
replacement for successful requests, and documents conditional `If-Match` and
`If-None-Match` writes for general-purpose buckets. Its conditional write API
also distinguishes `412 Precondition Failed` from concurrency-related `409` or
`404` outcomes. Network/SDK failures can still lose the response after S3 has
accepted a write, so adapter retry and error classification are correctness
decisions rather than performance policy.

## Goals

- Add `crates/w9pt-fs-storage-s3` without adding the AWS SDK, Tokio, HTTP, TLS,
  credentials, or provider behavior to `w9pt-fs-storage`, `w9pt-fs`, or `w9pt`.
- Use the official `aws-sdk-s3` client so conditional headers, response metadata,
  streaming bodies, and SDK dispatch failures remain visible to the adapter.
- Accept a caller-created S3 client and checked bucket configuration; do not load
  environment credentials, shared config files, endpoints, or regions in the
  adapter library.
- Pin the maintained SDK dependency graph and verify it explicitly under the
  workspace Rust 1.94.1 baseline selected after the dependency audit gate.
- Pass repository-generated `ObjectKey` values verbatim as private S3 object
  keys without introducing a visible path namespace or a second prefix owner.
- Represent `ObjectVersion` as a bounded, canonical, adapter-tagged ETag token;
  never interpret ETag as MD5 or as the repository's content digest.
- Implement `get` using size/ETag metadata followed by a version-conditional,
  bounded streamed body read.
- Implement exact half-open ranges with checked HTTP range conversion and strict
  `Content-Range`, `Content-Length`, ETag, and byte-count validation.
- Implement immutable creation with one retry-disabled `PutObject` carrying
  `If-None-Match: *`.
- Implement compare-and-swap with one retry-disabled conditional `PutObject`
  carrying `If-None-Match: *` or `If-Match: <etag>`.
- Classify definite conflict, definite adapter error, missing object, corruption,
  and potentially committed mutation separately.
- Extend `PutIfAbsent` with an explicit ambiguous outcome and resolve immutable
  write ambiguity through bounded exact readback before any manifest can depend
  on the object.
- Preserve upper-layer retry ownership: the S3 adapter never rebases a file
  mutation or publishes metadata.
- Provide offline request/fault tests, reusable target/content conformance, two
  independently constructed clients, pinned compatibility jobs, and opt-in live
  AWS qualification.
- Document minimum permissions, private-prefix policy, TLS/signing, lifecycle,
  storage-class, versioning, checksum, and operational requirements.

## Scope

### In scope

- Workspace membership and package policy for `w9pt-fs-storage-s3`.
- An exact pin to `aws-sdk-s3` 1.145.0 for the Rust 1.94.1 baseline, with default
  features disabled and only the required Tokio/default-HTTPS features exposed
  deliberately.
- Caller-owned `aws_sdk_s3::Client` construction and adapter-owned read/write
  client configuration derived from it.
- Checked bucket name, expected bucket owner, requester-pays flag, ETag length,
  body/range limit, conditional-resolution bound, and provider profile values.
- Amazon S3 general-purpose buckets using Signature Version 4 and immediately
  readable storage classes.
- Opt-in compatibility qualification for a named S3-compatible
  provider/version/endpoint configuration.
- `HEAD` plus conditional `GET` for complete objects, including pre-body size
  rejection and bounded streaming.
- Exact one-range `GET`, including empty range, missing object, beyond-EOF,
  ignored-range, malformed-response, and short/long body handling.
- Conditional single-part `PutObject` for immutable creation and CAS.
- Canonical strong ETag extraction/encoding/decoding and response consistency
  checks.
- Transfer checksum or signed-payload integrity supported by the selected SDK
  profile, while retaining `w9pt-fs-storage` BLAKE3 verification as the
  persistent content authority.
- Per-operation mutation retry suppression and explicit `SdkError`/HTTP/service
  outcome classification.
- Minimum runtime IAM behavior: prefix-scoped `s3:GetObject`, `s3:PutObject`, and
  `s3:ListBucket` needed to distinguish absent objects from access denial.
- Backend-neutral conformance extensions for ambiguity, missing/empty/exact
  ranges, concurrent writers, streaming limits, and independent clients.
- Mock HTTP/Smithy tests, fault tests, pinned emulator/provider jobs, and opt-in
  live AWS tests with test-owned namespaces and explicit cleanup controls.

### Out of scope

- Bucket creation, deletion, policy changes, credential discovery, role
  assumption, region discovery, endpoint discovery, secret management, or
  default application configuration.
- A daemon, CLI, listener, transport gateway, 9P session runner, or filesystem
  semantic engine integration executable.
- S3 directory buckets, S3 Express session authorization, S3 on Outposts,
  access-point/Multi-Region Access Point behavior, or endpoints containing an
  unsupported URL path prefix.
- Claiming support for MinIO, LocalStack, Ceph, Cloudflare R2, or another provider
  merely because its API is S3-compatible; each exact provider profile requires
  separate passing evidence.
- Multipart upload, transfer manager, append-object APIs, object composition,
  server-side copy, batch operations, or resumable upload state.
- Listing or deletion in the hot `TargetStore` path, garbage collection,
  lifecycle management, incomplete-upload cleanup, or object inventory.
- Caching, read-ahead, request coalescing, packed blocks, concurrency pools,
  adaptive throttling, or performance redesign of block-split storage.
- Client-side encryption, SSE-C key management, KMS policy management, Object
  Lock/retention management, ACL management, or replication configuration.
- Treating S3 ETag as a cryptographic content hash or trusting it instead of
  repository format/digest verification.
- A visible `path -> S3 key` mapping or any filesystem namespace metadata in S3.
- Use of `ObjectHeadPublisher` as a second authoritative per-file head when the
  clustered filesystem metadata database publishes `ContentRef`.
- Raising the workspace MSRV silently. The audit-triggered move from Rust 1.85
  to 1.94.1 and from `aws-sdk-s3` 1.96.0 to 1.145.0 was explicitly approved on
  2026-09-05; any later increase still requires an explicit decision.

## Affected Areas

| Area | Expected impact |
|---|---|
| `Cargo.toml` / `Cargo.lock` | Add the adapter member and pin the audited Rust-1.85-compatible SDK graph |
| `crates/w9pt-fs-storage-s3` | New S3 adapter, configuration, version codec, body bounds, error mapping, and tests |
| `crates/w9pt-fs-storage` | Add immutable-put ambiguity and extend reusable target conformance |
| `README.md` / `.dev/project.md` | Document the concrete S3 target, deployment contract, and remaining performance/runtime work |
| CI/test configuration | Add Rust 1.94.1, offline protocol, pinned compatibility, and opt-in live AWS gates |

## Acceptance Criteria

- `S3Target` implements every `TargetStore` operation and passes the reusable
  target conformance suite without weakening `TargetGuarantees::REQUIRED`.
- `cargo +1.94.1 check/test` succeeds with the exact SDK and committed transitive
  graph; dependency/license/security checks are recorded.
- Normal construction accepts an existing S3 client and never reads credentials,
  region, endpoint, shared config, environment configuration, or a global
  runtime itself.
- Complete reads reject negative/missing/oversized content length and invalid or
  missing ETag before body collection, condition the body read on that ETag, and
  reject any body/metadata mismatch.
- Range reads return exactly `[start,end)`, special-case empty ranges safely, and
  reject clamped, ignored, malformed, short, long, or overflowing responses.
- Every conditional write dispatches exactly one HTTP mutation attempt from the
  adapter; SDK retries cannot turn a committed response-lost CAS into a false
  `412` conflict.
- `If-None-Match: *` never replaces an existing current object, and concurrent
  immutable creators expose one value that is verified byte-for-byte.
- `If-Match` replaces only the current ETag. `412` becomes a definite conflict;
  `409`/present-CAS `404` perform bounded current-state classification; possible
  response loss becomes `Ambiguous`.
- A successful `PutObject`, `GET`, or `HEAD` yields a bounded canonical ETag
  token, and malformed or foreign `ObjectVersion` bytes are rejected before a
  request is sent.
- An ambiguous immutable write is read back: exact bytes succeed, different
  bytes report immutable collision, and unresolved absence/read failure remains
  typed ambiguity.
- Success is acknowledged only after S3 reports the write complete; subsequent
  independently configured clients immediately read the complete bytes.
- Missing objects return `None` only for a proven not-found response. Access
  denial, expected-owner mismatch, and endpoints lacking absence visibility are
  errors, never false absence.
- Offline tests cover all request headers, status mappings, body faults, response
  loss, and bounds without network credentials.
- Opt-in live tests use a caller-supplied general-purpose test bucket and unique
  private namespace, exercise two independent clients, and never target
  production data or delete outside their exact test prefix.
- Raw and block-split repository create/reopen/read/write/truncate/crash tests
  pass over the S3 adapter.
- No SDK/runtime dependency enters `w9pt`, `w9pt-fs`, `w9pt-fs-state`, or
  `w9pt-fs-storage`.

## Risks

| Risk | Mitigation |
|---|---|
| A conditional PUT commits but its response is lost | Disable SDK mutation retries and return explicit ambiguity for upper-layer exact readback |
| Automatic SDK retry changes a committed CAS into a later `412` | Derive a retry-disabled write client and assert one dispatched mutation in mock tests |
| HEAD and GET observe different object versions | Issue `GET If-Match` using the HEAD ETag and validate returned ETag/length |
| S3 clamps a range ending beyond EOF | Validate `Content-Range`, content length, and exact body size rather than accepting the response |
| Missing objects appear as `403` without list permission | Require prefix-scoped `s3:ListBucket`; never translate authorization failure to `None` |
| ETag is mistaken for MD5 or a digest | Treat it as a bounded opaque CAS token; use repository BLAKE3 and transfer checksums for integrity |
| External overwrite/delete or lifecycle action creates ABA/missing data | Reserve a private prefix, deny non-adapter mutation, and forbid expiration/archive transitions for reachable objects |
| An S3-compatible service ignores conditional headers | Require active conformance for the exact provider/version/configuration and fail closed |
| SDK response body outlives configured operation timeout | Bound collection and require explicit body/stall timeout behavior in adapter/client configuration |
| SDK maintenance requires a newer Rust baseline | Audit the locked graph continuously and require an explicit MSRV/dependency proposal before any later baseline increase |
| Versioned bucket delete markers change current-object semantics | Forbid external deletes and test/document enabled/suspended/unversioned behavior explicitly |
| Request checksums differ across compatible providers | Define a qualified checksum profile; never disable persistent BLAKE3 verification |
| Object-per-32-KiB block latency/cost is high | Keep this adapter correctness-first; benchmark and propose bounded parallelism/packing separately |

## Dependencies

- The implemented `w9pt-fs-storage::TargetStore`, `ObjectKey`, `ObjectVersion`,
  target guarantees, memory reference, and conformance suite.
- Exact `aws-sdk-s3` 1.145.0, whose crate metadata declares Rust 1.94.1 and whose
  S3 model exposes `PutObject::if_match` and `if_none_match`; all selected
  Smithy transitive versions remain locked and tested under Rust 1.94.1.
- Tokio execution through the SDK adapter crate only. Applications own the
  runtime and S3 client lifecycle.
- A caller-created S3 client configured with region, endpoint, SigV4
  credentials, TLS/HTTP connector, retry/timeouts, and any path-style setting.
- An Amazon S3 general-purpose bucket or a separately conformance-qualified
  provider profile with immediately readable objects and a private prefix.
- Runtime permissions for `GetObject`, `PutObject`, and prefix-scoped
  `ListBucket`; live-test cleanup additionally requires tightly scoped
  `DeleteObject`.
- Official behavior references: [Amazon S3 consistency](https://docs.aws.amazon.com/AmazonS3/latest/userguide/Welcome.html), [conditional writes](https://docs.aws.amazon.com/AmazonS3/latest/userguide/conditional-writes.html), [HeadObject](https://docs.aws.amazon.com/AmazonS3/latest/API/API_HeadObject.html), [GetObject](https://docs.aws.amazon.com/AmazonS3/latest/API/API_GetObject.html), and [AWS SDK for Rust retries](https://docs.aws.amazon.com/sdk-for-rust/latest/dg/retries.html).
