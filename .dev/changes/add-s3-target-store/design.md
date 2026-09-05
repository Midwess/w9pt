# Design: S3 Target Store

## 1. Responsibility Boundary

`w9pt-fs-storage-s3` maps the backend-neutral target contract onto S3. It owns:

- checked translation from `ObjectKey` and `ObjectRange` to S3 requests;
- bounded ETag-based `ObjectVersion` values;
- response metadata/body validation;
- conditional-write retry suppression;
- S3/SDK failure classification;
- provider conformance evidence.

It does not own:

- file layout, manifests, content digests, or logical EOF;
- visible paths or filesystem metadata;
- semantic write conflict/rebase behavior;
- database publication or writer fencing;
- client credentials, endpoint, region, TLS connector, or Tokio runtime;
- bucket creation, IAM, lifecycle, encryption, replication, or GC.

The host constructs the AWS S3 client, then constructs `S3Target`, then passes it
to `w9pt_fs_storage::ContentRepository`. `w9pt-fs-storage` remains free of all
SDK and runtime dependencies.

## 2. Supported Provider Contract

The production baseline is an Amazon S3 general-purpose bucket. AWS documents
strong read-after-write consistency for successful PUT/overwrite and atomic
single-key replacement, plus conditional writes using `If-None-Match` and
`If-Match`.

“S3-compatible” is a protocol family, not a semantic guarantee. A compatible
profile is supported only after the exact provider version and configuration
passes:

- immediate GET and HEAD after successful PUT;
- atomic `If-None-Match: *` under concurrent writers;
- atomic `If-Match` under concurrent replacements;
- stable bounded ETags on PUT/GET/HEAD;
- exact range and `Content-Range` behavior;
- successful-write durability evidence;
- response-lost/timeout fault behavior;
- checksum/signing, versioning, and delete-marker policy.

Directory buckets, S3 Express session authorization, S3 on Outposts, and
access-point variants are not included in the first profile. A provider that
ignores a conditional header fails closed even if all ordinary PUT/GET tests
pass.

## 3. SDK and Runtime Boundary

Pin `aws-sdk-s3` 1.145.0 after the original 1.96.0 / Rust 1.85 graph failed the
security gate. The replacement release declares Rust 1.94.1 and moves away from
the vulnerable or unsound transitive lines found by the audit. Pinning the
service crate does not freeze all Smithy dependencies, so the committed lockfile
and Rust 1.94.1 build are part of the supported contract.

Use default features off. Enable `rt-tokio`; expose the new default HTTPS client
as an adapter Cargo feature. Avoid the legacy HTTPS stack and SigV4a unless a
future provider profile requires them. Do not depend normally on `aws-config`:
applications can choose their own credential/config loader and pass the
constructed client.

The supplied client may use retries for idempotent reads. The adapter derives a
write client or per-operation override with SDK maximum attempts equal to one.
No environment variable or shared SDK config may re-enable mutation retries
after this override.

Construction first creates an unqualified candidate that advertises no writable
guarantees. Two independently configured candidates must pass live single-client
and concurrent target probes in a strictly checked test namespace before the
adapter returns qualified targets advertising `TargetGuarantees::REQUIRED`.
The AWS path also observes the serialized request endpoint and requires
verified HTTPS plus Amazon S3 bucket routing. Compatible endpoints may run the
same behavior probes without gaining AWS qualification.

## 4. Configuration

`S3TargetConfig` validates and retains:

- nonempty bounded bucket name/identifier;
- optional expected bucket owner;
- explicit requester-pays behavior;
- supported provider profile;
- maximum ETag bytes;
- adapter hard maximum body and range bytes;
- maximum read-only race retries;
- maximum current-state/ambiguity resolution reads;
- explicit streaming-body timeout/stall policy.

Endpoint URL, region, credentials, signing identity, TLS roots/provider,
addressing style, proxy, socket pool, and SDK read retry/backoff remain part of
the caller-owned client. The adapter documents that a custom endpoint containing
a path prefix is unsupported until the pinned SDK proves correct request
construction for it.

The adapter hard maximum may be greater than or equal to repository limits but
never permits an operation above its own bound. All `usize`/`u64`/SDK `i64`
conversions are checked before request construction.

## 5. Object Key Mapping

`ObjectKey` already contains the repository's private prefix and canonical
layout. The adapter uses that exact UTF-8 string as the S3 key. It does not add,
strip, URL-normalize, or reinterpret components.

The S3 SDK owns HTTP path encoding. Tests include spaces and safe punctuation
accepted by `ObjectKey` to prove one logical key maps identically across PUT,
HEAD, GET, Range GET, and conditional replacement.

Bucket identity is adapter routing and is not encoded into `ContentRef`. Moving
a repository to another bucket is an explicit deployment migration, not a
transparent reinterpretation.

## 6. Object Version Representation

S3 conditional PutObject uses ETag. Define one private versioned encoding:

```text
u8 adapter tag | u8 version | u16 ETag length | exact ETag bytes
```

Exact field widths are implementation details, but encoding must be canonical,
bounded, and independently golden-tested. The decoder rejects:

- wrong adapter tag or version;
- empty or oversized ETag;
- control/NUL bytes;
- inconsistent length or trailing bytes;
- values outside the provider profile's ETag syntax.

Preserve the exact quoting/case supplied by S3. Do not remove quotes and later
reconstruct them heuristically. Do not interpret ETag as MD5; multipart,
encryption, and compatible providers can use other ETag construction. The
repository's BLAKE3 digest remains the content-integrity authority.

S3 `versionId` is not included in the CAS predicate because PutObject `If-Match`
does not compare it. Advertising a token stronger than the operation actually
enforces would be incorrect. Versioning can remain enabled for operational
recovery if external mutation and delete markers are prohibited.

## 7. Complete Bounded Get

`get(key, max_bytes)` performs HEAD before GET to meet the contract's
pre-download limit:

1. Validate key and limits.
2. Send `HeadObject` with bucket, key, expected owner, and requester-pays fields.
3. Return `None` only for a proven not-found result.
4. Validate content length is present, nonnegative, convertible, and no greater
   than both `max_bytes` and the adapter hard bound.
5. Validate and encode the response ETag.
6. Send `GetObject If-Match: <head-etag>`.
7. Validate the GET ETag and length equal the HEAD observation.
8. Read body chunks with checked accumulation, hard maximum, and explicit
   timeout/stall behavior.
9. Require exact end-of-stream length.
10. Return `TargetObject` with the exact bytes and ETag token.

A `412` between HEAD and GET means a read-only version race. Retry the whole
sequence within `max_read_retries`; never combine metadata/body from different
versions. The private key model makes races unusual for immutable objects but
possible for the standalone mutable head.

For length zero, presence and ETag from HEAD are sufficient; the implementation
may avoid an empty body GET if tests prove the same version semantics.

## 8. Exact Range Get

`ObjectRange` is half-open. For nonempty `[start,end)`, construct the one allowed
HTTP range as `bytes=start-(end-1)` after checked subtraction. The requested
length must fit adapter/repository/platform bounds before I/O.

The response must prove:

- partial-range semantics rather than an ignored full-object response;
- the returned first/last byte equal the requested inclusive endpoints;
- a valid total object length;
- `Content-Length == end - start`;
- the streaming body contains exactly that many bytes;
- a valid ETag is present.

S3 can legally clamp an overlong range whose start is valid. The target contract
requires exact ranges, so clamping is an error. `416` is also a range error, not
absence. Only proven missing-object status yields `None`.

For `start == end`, issue HEAD to distinguish present and absent keys, returning
an empty vector only for proven presence.

## 9. Immutable Create-If-Absent

Send one single-part `PutObject` with:

```text
If-None-Match: *
Content-Length: exact byte count
expected owner/requester pays: configured
checksum/signing fields: provider profile
SDK attempts: one
```

Map outcomes:

| Observation | Target outcome |
|---|---|
| Successful response with valid ETag | `Created` |
| Single-attempt `412`, current HEAD has valid ETag | `AlreadyExists` |
| Explicit `409` | Bounded current-state classification; no blind mutation retry |
| Timeout/dispatch/408/429/5xx after possible dispatch | `Ambiguous` |
| Successful status without usable ETag | `Ambiguous` |
| Definite construction/auth/owner/permission/unsupported error | Adapter error |

The repository resolves `AlreadyExists` and `Ambiguous` by exact bounded bytes.
It must not store a manifest that depends on the object until exact desired bytes
are proven present.

## 10. Compare-and-Swap

Expected absence maps to one `If-None-Match: *` PutObject. Expected presence
first decodes the adapter ETag token, then maps to one `If-Match: <etag>`
PutObject.

| Observation | Target outcome |
|---|---|
| Successful response with valid ETag | `Replaced` |
| Single-attempt `412` | `Conflict` after current ETag lookup |
| `409` conditional conflict | Current-state classification, then `Conflict` when non-commit is proven |
| Present-CAS `404` | `Conflict { current: None }` when absence is proven |
| Possible response loss or malformed success metadata | `Ambiguous` |
| Definite local/auth/config error | Adapter error |

On conflict, the HEAD used for `current` can itself race; the returned token is
an observation, not a lock. The publisher will load/rebase against current state
with its own checked retry loop. If current state cannot be observed, return a
typed error after the original non-commit is proven or `Ambiguous` if commit
status is still uncertain.

The adapter never automatically repeats the conditional mutation. This prevents
a successful first attempt with a lost response from becoming a misleading
second-attempt `412`.

## 11. Immutable-Put Contract Repair

The backend-neutral contract must represent response loss after an immutable
conditional PUT:

```rust
pub enum PutIfAbsent {
    Created { version: ObjectVersion },
    AlreadyExists { version: ObjectVersion },
    Ambiguous,
}
```

The memory target maps an injected after-put failure to `Ambiguous`, not a
definitive error. Conformance verifies repository behavior for exact, different,
missing, and failed readback.

The ambiguity error distinguishes the operation/key and does not claim the
immutable object is absent. Failure before manifest publication can leave a late
unreachable object, which is safe for later GC.

## 12. Failure Classification

Classification is operation-sensitive:

| SDK/service observation | Read | Conditional write |
|---|---|---|
| Construction/configuration failure | Definite error | Definite error before dispatch |
| Authentication/authorization/expected owner | Definite error | Definite error |
| Proven object 404 | `None` | Conflict/current absence where applicable |
| 412 | Read-version race/error | Definite conditional conflict for one attempt |
| 409 | Read error | Classify current state; mutation request reported failed |
| 416 | Exact-range error | Not applicable |
| Dispatch timeout/connection reset | Read retry/error | Ambiguous |
| 408/429/5xx | Bounded read retry/error | Ambiguous after possible dispatch |
| Body stream failure | Definite failed read | Not applicable |
| 2xx missing/malformed ETag | Invalid response | Ambiguous for mutation |
| Body shorter/longer than metadata | Invalid response | Not applicable |

Error types retain the AWS request ID and extended request ID where available
and redact credentials, authorization, signed URLs, encryption context, and
object bytes.

## 13. Retry and Timeout Rules

- Mutation SDK attempts are exactly one.
- The adapter never retries or rebases file/content operations.
- HEAD/GET retries are bounded and apply only to idempotent reads or a HEAD/GET
  consistency race.
- Immutable ambiguity is resolved by repository exact readback.
- CAS ambiguity is resolved by `ObjectHeadPublisher` exact desired-head readback.
- Filesystem semantic conflict/ambiguity remains owned by `w9pt-fs` and the
  authoritative metadata ledger.
- No loop is unbounded, and every attempt count uses checked arithmetic.

AWS SDK operation timeouts do not cover consuming a returned ByteStream. The
adapter therefore requires explicit body timeout/stall behavior and tests a body
that never yields, yields too slowly, or fails after partial bytes. The caller
still owns the Tokio runtime and the configured duration values.

## 14. Durability and Integrity

A successful target PUT means the provider has durably accepted the complete
object according to its qualified profile. The adapter does not acknowledge
before the SDK returns a successful complete operation result and a valid ETag.

Use the qualified SDK transfer checksum or signed-payload behavior. Full
repository objects also contain checked envelopes and BLAKE3 digests, which are
verified on later reads. ETag supplies concurrency identity only.

Objects must remain immediately readable. Lifecycle expiration and transitions
to archival storage are forbidden for any reachable private-prefix object.
Server-side default SSE-S3/SSE-KMS can be deployment policy; SSE-C is excluded
because the adapter would need to own secret headers on every request.

## 15. Absence and Permissions

Amazon S3 may return `403` rather than `404` for a missing object when the caller
lacks `s3:ListBucket`. Since `TargetStore` distinguishes `None` from adapter
failure, runtime roles need:

```text
s3:GetObject on bucket/private-prefix/*
s3:PutObject on bucket/private-prefix/*
s3:ListBucket on bucket restricted to private-prefix/*
```

Live-test cleanup additionally needs `s3:DeleteObject` limited to the exact test
namespace. Runtime hot paths do not require delete or general listing.

Bucket policies should require conditional headers for mutable publication keys
and prevent outside writers from overwriting/deleting the prefix. The adapter
does not rely on ACLs or expose public objects.

## 16. Testing and Qualification

Testing layers are distinct:

1. Pure unit tests validate config, ETag codecs, ranges, content-range parsing,
   body bounds, status classification, and redaction.
2. Mock HTTP/Smithy tests capture exact method/bucket/key/headers/body and inject
   status, response, stream, timeout, and dispatch failures.
3. Shared conformance verifies the backend-neutral target contract.
4. Repository tests run raw/block-split preparation, publication, crash, and
   reopen flows over an S3-backed test client.
5. Pinned compatibility services demonstrate only their named profiles.
6. Opt-in live AWS tests qualify Amazon S3 general-purpose behavior using two
   separately constructed clients and an isolated caller-provided namespace.

No test provisions a bucket automatically. Required-mode environment gating
fails when live credentials/config are missing; ordinary local tests skip cleanly.
Cleanup validates the resolved bucket and exact test prefix before deletion.
