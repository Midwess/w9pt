# Analysis: Add S3 Target Store

## Current State

- The workspace now names the backend-neutral content crate
  `w9pt-fs-storage`; its package and Rust import are
  `w9pt-fs-storage`/`w9pt_fs_storage`.
- `TargetStore` has four owned asynchronous operations: bounded exact `get`,
  exact half-open `get_range`, atomic `put_if_absent`, and single-key
  `compare_exchange`.
- `TargetGuarantees::REQUIRED` requires durable successful writes, atomic
  create-if-absent, atomic CAS, and read-after-write visibility. Repository
  construction rejects weaker target stores.
- The memory target and reusable target conformance suite exist. There is no
  network/provider adapter in the workspace.
- The current repository limits default to a 64 MiB maximum stored object, well
  below S3's single-request upload scale. The public API already owns complete
  request bytes, so multipart streaming is not required for this first adapter.
- The worktree contains active user changes for the crate rename, semantic
  engine, state model, and PostgreSQL adapter. This proposal must add only
  `.dev/changes/add-s3-target-store/` and must not normalize or overwrite those
  changes.
- `.dev/project.md` exists, so no dev-workflow bootstrap is required. There is no
  archived/current `.dev/specs` directory; related normative deltas remain in
  their change directories.

## Existing Contract Evidence

### Target operations

`w9pt-fs-storage/src/object_store.rs` defines:

```text
get(key, max_bytes) -> optional exact bytes + opaque version
get_range(key, [start,end)) -> optional exact bytes
put_if_absent(key, bytes) -> created or already exists
compare_exchange(key, expected version/absence, bytes)
    -> replaced, conflict, or ambiguous
```

The repository already verifies an `AlreadyExists` immutable object by loading
it within configured bounds and comparing exact encoded bytes. It already
resolves ambiguous mutable-head CAS through the `ObjectHeadPublisher` readback
rules. Clustered filesystem operation does not use that object head as metadata
authority; the database publishes `ContentRef`.

### Reusable conformance

`check_target_conformance` currently verifies:

- immutable create/no-replace;
- immediate exact read and opaque version equality;
- complete-read size bounds;
- exact and out-of-bounds ranges;
- CAS create, conflict, replacement, and version change;
- immediate read-after-publication.

It does not yet cover missing/empty ranges, concurrent independent clients,
response-lost immutable PUT, 409/404 races, malformed/missing versions, ignored
ranges, streaming body overrun/failure, or retry-attempt counts. Backend-neutral
cases should extend the shared suite; protocol-specific cases belong in the S3
crate.

## SDK Selection

The official AWS SDK is preferred over a generic object-store abstraction for
this adapter because exact conditional headers and `SdkError` dispatch/service
classification are part of the correctness boundary.

As checked for this proposal:

- the initial `aws-sdk-s3` 1.96.0 / Rust 1.85 graph failed the 2026-09-05
  security gate because its last Rust-1.85-compatible `time` release was
  vulnerable and its `lru 0.12.5` dependency carried unsoundness advisories;
- the explicitly approved replacement `aws-sdk-s3` 1.145.0 declares
  `rust-version = 1.94.1` and uses maintained dependency lines;
- 1.145.0 exposes `PutObject::if_match`, `if_none_match`, per-operation
  customization, `HEAD`/`GET` ETag and content-length fields, conditional reads,
  range requests, and streaming response bodies.

Pin `aws-sdk-s3 = "=1.145.0"`, disable default features, and expose only the
Tokio/default-HTTPS features intentionally. The application supplies the client;
`aws-config` is not a normal dependency. Exact service pinning is insufficient
by itself because Smithy dependencies use compatible ranges, so commit the
resolved graph and run the complete crate on `cargo +1.94.1`.

The aging SDK pin is a real maintenance risk. Dependency auditing is an
acceptance gate. If the locked graph has an unresolvable advisory or no longer
meets provider requirements, implementation stops for an explicit MSRV or SDK
decision rather than weakening conditional semantics.

## Official S3 Behavior

Amazon S3 documents for general-purpose buckets:

- strong consistency for successful object PUT/overwrite/delete and subsequent
  GET/HEAD, with atomic updates to one key;
- `If-None-Match: *` create-if-absent and `If-Match: ETag` conditional
  replacement;
- `412` when the precondition does not match and possible `409`/`404` outcomes
  under conflicting deletes/writes;
- ETag as an opaque identifier for a resource version rather than a universal
  content digest;
- one range per GET and response `Content-Range`/`Content-Length` metadata;
- `HEAD` returning `404` for a missing object only when the caller also has
  `s3:ListBucket`; otherwise absence may appear as `403`.

Relevant primary sources:

- [Amazon S3 consistency model](https://docs.aws.amazon.com/AmazonS3/latest/userguide/Welcome.html)
- [S3 conditional writes](https://docs.aws.amazon.com/AmazonS3/latest/userguide/conditional-writes.html)
- [S3 conditional reads](https://docs.aws.amazon.com/AmazonS3/latest/userguide/conditional-reads.html)
- [HeadObject API](https://docs.aws.amazon.com/AmazonS3/latest/API/API_HeadObject.html)
- [GetObject API](https://docs.aws.amazon.com/AmazonS3/latest/API/API_GetObject.html)
- [AWS SDK for Rust retry configuration](https://docs.aws.amazon.com/sdk-for-rust/latest/dg/retries.html)
- [AWS SDK for Rust per-operation configuration](https://docs.aws.amazon.com/sdk-for-rust/latest/dg/peroperation.html)
- [AWS SDK for Rust source and MSRV policy](https://github.com/awslabs/aws-sdk-rust)

## Contract Gap: Immutable PUT Ambiguity

`CompareExchange` already has `Ambiguous`, but `PutIfAbsent` does not and the
associated adapter error is documented as definitive. A conditional S3 PUT may
be accepted durably while its response is lost. Treating that as definitive
would misstate the target contract.

Add:

```rust
pub enum PutIfAbsent {
    Created { version: ObjectVersion },
    AlreadyExists { version: ObjectVersion },
    Ambiguous,
}
```

On `Ambiguous`, immutable repository preparation performs a bounded exact
readback:

- desired bytes are present: treat the dependency as durable;
- other bytes are present: report `ImmutableCollision`;
- absence or failed/uncertain readback: return typed unresolved ambiguity and do
  not upload/publish the dependent manifest.

A late immutable object is safe but unreachable. It must never cause metadata to
reference data whose durability was not established.

## Proposed S3 Mapping

### Object keys and configuration

The repository's `KeySpace` already owns the complete private key prefix.
`S3Target` sends `ObjectKey::as_str()` verbatim as the S3 key. Adding another
adapter prefix would make key ownership and persisted references ambiguous.

`S3TargetConfig` contains bounded bucket and request behavior, not credentials or
endpoint discovery. The caller supplies a configured client. An optional
expected bucket owner is sent on every applicable request. Requester-pays support
is explicit rather than environment-derived.

### ETag version tokens

Use a private canonical tagged encoding around the exact returned ETag. The
decoder rejects empty, oversized, control-containing, noncanonical, or foreign
tokens before dispatch. Preserve quoting/case exactly as expected by S3
conditional requests.

Do not include `versionId` as if it were the `If-Match` predicate: S3 conditional
PutObject evaluates ETag. Bucket versioning may be enabled for operational
recovery, but external overwrites/deletes and delete markers remain forbidden in
the private prefix.

### Complete get

1. `HeadObject` obtains content length and ETag without a body.
2. Reject missing/negative/overflowing length, missing/invalid ETag, or a length
   above `max_bytes` before issuing the body GET.
3. Issue `GetObject` with `If-Match` for that ETag.
4. Validate response ETag and length against HEAD.
5. Collect the body chunk by chunk with a hard byte bound and explicit body
   timeout/stall policy.
6. Require exact end-of-stream length.
7. If the object changes between HEAD and GET, retry the read-only sequence
   within a checked bound or return a typed error.

This adds one metadata request per complete read but is required by the current
pre-download size contract. A later contract/performance proposal may introduce
a trustworthy streamed-size alternative.

### Range get

Map non-empty `[start,end)` to the inclusive HTTP header
`bytes=start-(end-1)` using checked arithmetic. Validate the returned
`Content-Range`, content length, ETag presence, and exact body size. S3 may return
a shorter clamped body when the end exceeds EOF; this is an error under
`TargetStore`, not a successful clamp. Empty ranges use `HEAD` so present-empty
and absent objects remain distinguishable.

### Immutable put

Issue one retry-disabled, single-part `PutObject` with `If-None-Match: *`, exact
content length, and the selected transfer-integrity profile. A successful reply
must contain a valid ETag. `412` means the key is current and produces
`AlreadyExists` after its ETag is obtained. Dispatch/timeout/5xx/response-parse
uncertainty produces `Ambiguous`. `409` is classified through bounded current
state rather than blind retry.

### Compare and exchange

- expected absence: `PutObject If-None-Match: *`;
- expected version: decode the token and send `PutObject If-Match: <etag>`;
- successful reply: require a new valid ETag and return `Replaced`;
- single-attempt `412`: definite `Conflict`, with bounded HEAD to obtain current
  ETag;
- concurrent `409` or present-CAS `404`: classify against current state;
- timeout, dispatch uncertainty, 408/429/5xx, or malformed/incomplete successful
  response after possible dispatch: `Ambiguous`.

The adapter never performs semantic rebase. `ObjectHeadPublisher` resolves a CAS
ambiguity by exact head readback; `w9pt-fs` resolves filesystem mutation conflicts
through the metadata transaction.

## Retry and Timeout Ownership

The AWS SDK normally retries some transient errors. Retrying `If-Match`
transparently is unsafe: attempt one can commit and lose its response, while an
automatic attempt two receives `412` against the now-stale ETag. The adapter
derives or overrides a write configuration with one SDK attempt and verifies
that mock requests dispatch once.

Read-only HEAD/GET may use caller-configured bounded retries. Conditional write
recovery is explicit and bounded above the SDK. The adapter has a separate body
timeout/stall contract because SDK operation timeout does not cover consuming a
returned streaming body.

## Provider and Deployment Qualification

The first production profile is Amazon S3 general-purpose buckets. Directory
buckets and S3 Express authorization are excluded. A custom endpoint must pass
the same exact request, concurrency, ambiguity, and durability suite for the
named provider version/configuration.

Runtime policy must ensure:

- private prefix access only;
- `GetObject`, `PutObject`, and prefix-scoped `ListBucket`;
- conditional headers cannot be bypassed for mutable keys;
- no external overwrite/delete under the prefix;
- no lifecycle expiration or archival transition for reachable objects;
- TLS certificate verification and Signature Version 4;
- immediately readable storage class;
- transfer-checksum behavior compatible with the selected client/provider.

The library documents policies but does not provision or mutate them.

## Affected Files

```text
Cargo.toml
Cargo.lock
README.md
.dev/project.md

crates/w9pt-fs-storage/
  src/object_store.rs
  src/error.rs
  src/repository.rs
  src/testing/conformance.rs
  src/testing/memory_store.rs
  tests/object_store_conformance.rs

crates/w9pt-fs-storage-s3/
  Cargo.toml
  README.md
  src/lib.rs
  src/config.rs
  src/store.rs
  src/error.rs
  src/version.rs
  src/body.rs
  src/range.rs
  src/testing.rs
  tests/request_mapping.rs
  tests/bounded_reads.rs
  tests/conditional_writes.rs
  tests/ambiguity.rs
  tests/s3_conformance.rs
  tests/common/mod.rs
```

## Conventions

- Rust 2024, Rust 1.94.1, Apache-2.0, `unsafe_code = "forbid"`, complete public
  documentation, and workspace Clippy policy.
- Checked conversion before every S3 length, byte range, ETag, key, body, or
  retry calculation.
- Typed errors with request IDs but no credentials, authorization headers,
  signed URLs, encryption context, or response bodies in normal diagnostics.
- Caller-owned client, credentials, endpoint, region, TLS, runtime, and
  deployment controls.
- No visible path names in target keys and no object listing for normal file or
  directory operations.
- No AGPL ZeroFS code, tests, comments, formats, or distinctive structure.

## Risks and Dependencies

- The old exact AWS SDK pin requires continuous audit and may force a separate
  MSRV decision sooner than the core crates.
- Conditional behavior varies materially across S3-compatible providers and can
  be accepted but ignored. Only live concurrent/fault tests prove the adapter's
  advertised guarantees for a provider profile.
- ETag is the S3 CAS predicate but not an end-to-end content hash. Persistent
  BLAKE3 checks remain mandatory.
- Complete `get` uses HEAD plus conditional GET, adding request latency and cost.
- Current block-split layout performs many small object requests sequentially.
  This proposal establishes correctness, not production throughput optimization.
- Live tests require a dedicated bucket/prefix and permissions and must remain
  opt-in unless CI provides explicitly disposable infrastructure.
