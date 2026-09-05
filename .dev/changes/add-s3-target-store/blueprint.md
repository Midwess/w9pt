# Implementation Blueprint: Add S3 Target Store

## Design Approach

Create a separate runtime-specific adapter crate:

```text
w9pt-fs
  -> w9pt-fs-storage::ContentRepository<S3Target>
       -> w9pt-fs-storage-s3::S3Target
            -> caller-owned aws_sdk_s3::Client
                 -> Amazon S3 general-purpose bucket
```

Dependency direction is strictly downward:

```text
w9pt-fs-storage-s3
  -> w9pt-fs-storage
  -> aws-sdk-s3
```

No core crate depends on the S3 adapter, AWS SDK, Tokio, HTTP, TLS, or credential
types. The adapter implements the existing `TargetStore` semantics and exposes
the provider-specific failure detail needed by the embedding application.

## SDK and Feature Policy

Use the official AWS SDK for Rust because the implementation must control exact
`If-Match`/`If-None-Match` headers, inspect ETag/content-range metadata, disable
mutation retries, and classify `SdkError` variants.

Initial dependency policy:

```toml
[dependencies]
w9pt-fs-storage = { path = "../w9pt-fs-storage" }
aws-sdk-s3 = { version = "=1.145.0", default-features = false, features = ["rt-tokio"] }

[features]
default = ["default-https-client"]
default-https-client = ["aws-sdk-s3/default-https-client"]
```

The exact feature spelling must be confirmed in the feasibility task. Do not
enable the legacy HTTPS stack or SigV4a unless a supported provider profile
requires it. Do not add `aws-config` as a normal dependency. The host constructs
the client with its credential, region, endpoint, TLS, timeout, retry, and
addressing choices.

`aws-sdk-s3` 1.145.0 is selected after the initial 1.96.0 graph failed the
security audit. It declares Rust 1.94.1 and includes the required conditional
PutObject inputs. Commit the entire resolved dependency graph, run Rust 1.94.1
CI, and audit it. Any later MSRV increase requires another explicit decision.

## Public API

The exact shape can be refined during implementation while preserving this
ownership model:

```rust
pub struct S3Target {
    read_client: aws_sdk_s3::Client,
    write_client: aws_sdk_s3::Client,
    config: S3TargetConfig,
}

pub struct S3TargetConfig {
    bucket: String,
    expected_bucket_owner: Option<String>,
    requester_pays: bool,
    provider: S3ProviderProfile,
    max_etag_bytes: usize,
    max_body_bytes: usize,
    max_read_retries: u32,
    max_resolution_attempts: u32,
    body_timeout: BodyTimeout,
}

pub enum S3ProviderProfile {
    AwsGeneralPurpose,
    QualifiedCompatible(ValidatedProviderProfile),
}

impl S3Target {
    pub fn new(
        client: aws_sdk_s3::Client,
        config: S3TargetConfig,
    ) -> Result<Self, S3ConfigurationError>;
}
```

`S3Target::new` produces an unqualified candidate whose advertised guarantees
are all false. `S3Target::qualify_pair` consumes two independently configured
candidates plus a strictly test-scoped namespace, runs the reusable single and
concurrent target probes, and returns qualified targets only after every probe
succeeds. `ContentRepository` therefore rejects an unqualified client before
writable use. AWS qualification also observes the serialized endpoint and
requires verified HTTPS plus Amazon S3 bucket routing. Compatible endpoints may
run `probe_pair`, but that operation never grants guarantees or creates a
supported profile.

Fields remain private behind checked constructors/getters. `ValidatedProviderProfile`
is produced only by a conformance/qualification API or represents explicit
deployment evidence; arbitrary custom endpoints cannot silently claim required
guarantees.

`S3Target` clones or derives the supplied client configuration into:

- a read client retaining explicit caller-configured bounded read retries;
- a write client with SDK retries disabled for every conditional mutation.

If the SDK cannot reliably derive a retry-disabled client, every conditional
operation uses a per-operation configuration override and tests assert exactly
one dispatched mutation.

## Core Contract Repair

Extend the backend-neutral immutable outcome:

```rust
pub enum PutIfAbsent {
    Created { version: ObjectVersion },
    AlreadyExists { version: ObjectVersion },
    Ambiguous,
}
```

Update `MemoryTarget` so an injected after-put failure returns `Ambiguous`.
Update `ContentRepository::put_immutable`:

```text
Created                         -> success
AlreadyExists                   -> bounded exact read and byte verification
Ambiguous + exact desired bytes -> success
Ambiguous + other bytes         -> ImmutableCollision
Ambiguous + absent/read failure -> unresolved immutable-put ambiguity
```

The repository never uploads the dependent manifest until the payload's exact
bytes are proven present. Extend the public ambiguity error so immutable-put and
mutable-head ambiguity are distinguishable without exposing adapter details.

## Key Mapping

`w9pt-fs-storage::KeySpace` already constructs the full private repository key.
The adapter sends `ObjectKey::as_str()` verbatim as the bucket key. It does not:

- prepend another hidden prefix;
- parse visible filesystem paths;
- normalize slash-separated segments;
- list keys for normal operations;
- expose bucket keys through 9P.

The bucket and expected owner are deployment routing, not part of persisted
`ObjectKey` or `ContentRef`.

## ETag Version Codec

Create a private canonical adapter token, for example:

```text
magic/version | ETag byte length | exact ETag bytes
```

The codec:

- has a small explicit maximum;
- accepts only the exact nonempty ETag returned by successful S3 operations;
- preserves quoting and case;
- rejects control bytes, invalid length, unknown version, trailing bytes, and
  tokens produced by another target adapter;
- never treats ETag as MD5, BLAKE3, or an object payload digest.

Only the ETag is used because S3 `PutObject If-Match` evaluates ETag. A returned
S3 version ID may be logged/observed separately but must not be encoded as a
stronger CAS predicate than the request actually enforces.

## Complete Object Read

Implement `TargetStore::get` as:

```text
1. Validate key and max_bytes against adapter hard ceilings.
2. HEAD object with expected-owner/requester-pays settings.
3. Map a proven 404 to None; keep 403/owner/auth failures as errors.
4. Validate nonnegative Content-Length and bounded canonical ETag.
5. Reject Content-Length > max_bytes before issuing GET.
6. GET object with If-Match set to the HEAD ETag.
7. Validate GET ETag and Content-Length equal HEAD.
8. Collect ByteStream incrementally with hard length and body timeout/stall
   enforcement.
9. Require exact end-of-stream length and return TargetObject(bytes, token).
10. On a read-only 412 race, repeat HEAD/GET within max_read_retries.
```

Do not use `ByteStream::collect` without an independent hard bound. Do not map a
missing `Content-Length` or ETag to a fabricated value.

## Exact Range Read

For nonempty `[start,end)`:

1. Check order, `end - start`, platform conversion, adapter maximum, and
   `end - 1`.
2. Send exactly `Range: bytes=<start>-<end-1>`.
3. Map only proven missing-object responses to `None`.
4. Require the response to identify the requested start/end and total object
   size in canonical `Content-Range` form.
5. Validate returned `Content-Length`, ETag, and body length exactly.
6. Reject ignored range (`200`/full body), clamped range, malformed content
   range, `416`, short/long body, or a body exceeding the requested length.

For an empty range, use HEAD to distinguish a present object from absence and
return `Some(Vec::new())` only when presence is proven.

## Immutable Conditional Put

Map `put_if_absent` to one single-part PutObject:

```text
If-None-Match: *
Content-Length: exact checked bytes length
expected bucket owner / requester pays: configured values
transfer checksum or signed payload: qualified profile
SDK write attempts: exactly one
```

Outcome rules:

- success with valid ETag -> `Created`;
- `412` plus current valid ETag -> `AlreadyExists`;
- possible response loss, dispatch timeout, 408/429/5xx after dispatch, or
  malformed success response -> `Ambiguous`;
- `409` -> read/classify current state within the resolution bound, never blind
  SDK retry;
- definitive construction, authentication, owner, permission, or unsupported
  request failure -> typed error.

## Compare and Exchange

Map expected absence to `If-None-Match: *` and expected presence to
`If-Match: <decoded-etag>`. Decode and validate the token before request
construction.

- success requires a canonical response ETag and returns `Replaced`;
- single-attempt `412` is definite non-commit and returns `Conflict` after
  bounded HEAD obtains the latest ETag;
- conditional `409` and present-CAS `404` are classified with current-state
  readback;
- any failure that may follow dispatch/commit returns `Ambiguous`;
- no adapter path repeats the conditional mutation or rebases file content.

The higher `ObjectHeadPublisher` compares desired head bytes after ambiguity.
Clustered `w9pt-fs` uses the metadata database instead and does not publish a
mutable S3 file head.

## Error Model

Define typed, redacted errors for:

- invalid configuration/bucket/profile;
- invalid/foreign/oversized ETag token;
- key, range, or numeric conversion failure;
- missing/negative/oversized response metadata;
- ignored, clamped, malformed, or inconsistent range;
- body timeout, stall, short read, overrun, or stream failure;
- authentication/authorization/expected-owner failure;
- unsupported bucket/provider behavior;
- definite S3 service failure;
- provider conformance failure.

Retain AWS request and extended request IDs when available. Do not format
credentials, authorization headers, signed URLs, SSE contexts, request bodies,
or response bodies into normal errors.

## Provider Qualification

`AwsGeneralPurpose` relies on documented AWS semantics and still runs live
adapter conformance before production release. Every compatible profile records:

```text
provider name and exact version
endpoint/addressing mode
conditional header behavior
consistency and durability evidence
range behavior
ETag behavior
checksum profile
versioning/delete-marker policy
test timestamp/build evidence
```

Qualification uses a unique caller-supplied test namespace. Cleanup is an
explicit test/admin operation with a separate tightly scoped delete permission;
delete/list never enter the hot `TargetStore` interface.

## Permissions and Bucket Policy

Runtime documentation requires:

- `s3:GetObject` and `s3:PutObject` limited to the repository prefix;
- `s3:ListBucket` restricted by prefix so missing objects produce a distinguishable
  not-found result rather than an authorization-shaped 403;
- expected bucket owner where applicable;
- a policy preventing unconditional overwrite of mutable adapter keys;
- no external writer/deleter, lifecycle expiration, or archival transition for
  reachable private objects;
- verified HTTPS and Signature Version 4.

The adapter validates observable behavior but does not create or edit IAM,
bucket, lifecycle, encryption, replication, or network policy.

## Files to Create or Modify

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
  src/
    lib.rs
    config.rs
    store.rs
    error.rs
    version.rs
    range.rs
    body.rs
    classify.rs
    testing.rs
  tests/
    request_mapping.rs
    bounded_reads.rs
    conditional_writes.rs
    ambiguity.rs
    s3_conformance.rs
    common/mod.rs
```

## Implementation Phases

### Phase 1: Repair immutable ambiguity

Add `PutIfAbsent::Ambiguous`, typed immutable ambiguity, repository readback,
memory failure behavior, and backend-neutral conformance tests.

### Phase 2: SDK feasibility and crate foundation

Prove the exact dependency/features and write-client retry override on Rust 1.94.1,
audit the graph, add package policy, checked configuration, and caller-owned
client construction.

### Phase 3: Version, range, body, and error primitives

Implement golden ETag tokens, checked range headers/content-range parsing,
bounded timed body collection, redacted errors, and metadata validation.

### Phase 4: Read operations

Implement HEAD/conditional complete GET and exact range GET with all missing,
race, bound, malformed, and stream-failure cases.

### Phase 5: Conditional mutations

Implement single-attempt immutable PutObject and CAS, precise 412/409/404
classification, ambiguity, current-version lookup, and transfer integrity.

### Phase 6: Offline conformance

Capture mock HTTP requests, prove mutation attempt counts, inject transport/body
faults, extend target conformance, and run raw/block-split repository tests over
the adapter test double.

### Phase 7: Provider qualification and documentation

Run pinned compatible-provider jobs and opt-in live AWS tests with two clients,
publish exact supported profiles and IAM/bucket guidance, update project status,
and run all workspace validation gates.

## Testing Strategy

- Unit/golden tests for configuration, bucket/key forwarding, version tokens,
  range conversion, content-range parsing, metadata bounds, and redaction.
- Captured Smithy/mock HTTP requests proving exact bucket/key, expected owner,
  requester-pays, checksum, Range, If-Match, If-None-Match, and one-attempt write
  behavior.
- A response body that fails if polled, proving oversized complete GET stops
  after HEAD and before body request/collection.
- Body tests for exact, empty, short, long, chunk-overrun, stalled, and midstream
  failure cases.
- Status/error tables covering proven 404, ambiguous 403, 408, 409, 412, 416,
  429, 5xx, construction, timeout, dispatch, response parsing, and missing ETag.
- Response-lost tests where immutable readback proves exact bytes and where
  object-head readback proves or cannot prove a desired CAS.
- Two independent client races for immutable creation and compare-and-swap.
- Reusable target conformance plus raw/block-split content create/reopen/read/
  write/truncate/publication/crash suites.
- Optional provider jobs that fail closed if conditional headers are ignored.
- Opt-in live AWS general-purpose bucket tests with explicit required-mode
  environment gating and exact test-prefix cleanup.
- `cargo +1.94.1` build/tests, current-toolchain workspace tests, rustfmt, Clippy
  with warnings denied, license/dependency/security checks, and public docs.
