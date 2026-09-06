# w9pt-fs-storage-s3

`w9pt-fs-storage-s3` maps the runtime-neutral
`w9pt_fs_storage::TargetStore` contract onto a caller-configured Amazon S3
client. It is intentionally separate from protocol, filesystem-semantic,
metadata, and content-layout crates.

> **Development status:** This crate is unreleased. Its API, qualification
> contract, and adapter token encodings may change without compatibility support
> for earlier builds.

The production profile is an Amazon S3 general-purpose bucket. An
S3-compatible endpoint is not writable merely because it accepts S3-shaped
requests. Its exact provider version, endpoint/addressing mode, conditional
writes, consistency, durability, ETags, ranges, checksums, versioning, and fault
behavior must pass qualification before a profile can be added. Version 0.1 has
no qualified compatible-provider profile and fails such requests closed.

## Construction and ownership

The application constructs two independent `aws_sdk_s3::Client` values, then
passes each one and a checked `S3TargetConfig` to `S3Target::new`. A new target
is an unqualified candidate and advertises no writable guarantees. The pair must
pass `S3Target::qualify_pair` in a checked caller-owned test namespace before a
`ContentRepository` can accept it. The adapter does not read environment
credentials, shared AWS files, region, endpoint, retry, proxy, TLS, or addressing
configuration and does not create a Tokio runtime.

AWS qualification observes the actual serialized request and requires verified
HTTPS plus an Amazon S3 hostname/path addressing the configured bucket. A custom
endpoint cannot acquire AWS writable guarantees by passing behavioral probes.
`S3Target::probe_pair` may exercise a compatible endpoint, but it never marks
that target qualified or creates a supported provider profile.

```rust,no_run
use aws_sdk_s3::Client;
use w9pt_fs_storage_s3::{S3QualificationNamespace, S3Target, S3TargetConfig};

async fn targets(
    first_client: Client,
    second_client: Client,
) -> Result<(S3Target, S3Target), Box<dyn std::error::Error>> {
    let config = S3TargetConfig::builder("private-w9pt-bucket")
        .expected_bucket_owner("012345678901")
        .max_body_bytes(64 * 1024 * 1024)
        .max_range_bytes(64 * 1024 * 1024)
        .build()?;
    let namespace = S3QualificationNamespace::new(
        "qualification/w9pt-s3-test-0123456789abcdef",
    )?;
    let first = S3Target::new(first_client, config.clone())?;
    let second = S3Target::new(second_client, config)?;
    Ok(S3Target::qualify_pair(first, second, &namespace).await?)
}
```

The repository's `ObjectKey` already contains its full private prefix. The
adapter forwards that text verbatim; it never prepends a second prefix or maps
visible filesystem paths to S3 keys.

## Runtime IAM and bucket policy

The runtime role needs only:

- `s3:GetObject` for `arn:aws:s3:::BUCKET/PRIVATE_PREFIX/*`;
- `s3:PutObject` for the same resource;
- `s3:ListBucket` on `arn:aws:s3:::BUCKET`, restricted by an `s3:prefix`
  condition to `PRIVATE_PREFIX/*`, so missing objects are distinguishable from
  access denial.

A `403` is never treated as absence. Set `expected_bucket_owner` so a wrong
account fails instead of becoming an alternate target. Bucket policy should
deny writes that bypass `If-None-Match: *` for immutable data/manifests and
deny mutable-head writes that carry neither the expected `If-Match` nor
`If-None-Match` condition. Clustered filesystem operation publishes
`ContentRef` in the authoritative metadata database and must not use the
standalone S3 object head as a second authority.

The runtime hot path needs no `ListObjects`, `DeleteObject`, bucket creation,
policy mutation, or lifecycle mutation. Live-test cleanup uses separate
prefix-scoped `s3:DeleteObject` permission. Cleanup requires an expected account
owner, a bucket name containing `w9pt-test`, and a final namespace component of
`w9pt-s3-test-` plus at least sixteen run-ID bytes.

## Transport, credentials, and integrity

Use verified HTTPS and Signature Version 4. The caller owns credential sourcing,
refresh, role assumption, TLS roots/provider, proxies, endpoint selection, and
path-style configuration. Anonymous or insecure fallback is unsupported.
Custom endpoints, including endpoints containing a URL path prefix, cannot use
the Amazon S3 qualification path. A future compatible-provider profile requires
separate accepted durability and fault evidence.
Directory buckets, S3 Express session authorization, S3 on Outposts, access
points, and Multi-Region Access Points are outside the version-0.1 profile.

Conditional uploads use single-part `PutObject`, an exact content length,
CRC32C SDK transfer integrity, and one SDK mutation attempt. The SDK ETag is an
opaque concurrency token only; it is never interpreted as MD5 or trusted as the
content digest. `w9pt-fs-storage` v3 authenticated representation and BLAKE3
digests remain the persistent integrity authority. Optional client-side
AES-SIV is separate from bucket-default server-side encryption; the adapter
sees only already-protected object bytes and never receives the master KEK or
per-file DEK.

Bucket-default SSE-S3 or SSE-KMS is deployment policy. SSE-C is excluded because
it would make the adapter own secret headers on every read and write. Normal
errors retain only bounded safe request identifiers and classifications; they
omit credentials, authorization, signed URLs, encryption context, SDK messages,
and object bytes.

## Reachability and lifecycle requirements

Reserve the private prefix exclusively for this repository. External writers,
unconditional overwrites, deletes, delete markers, restores, or object
replacement can break immutable reachability and are unsupported. Lifecycle
expiration and transitions to archival or otherwise non-immediately-readable
storage are forbidden for reachable objects.

Bucket versioning may be used for operational recovery only when outside writes
and delete markers remain prohibited. The simple live-test cleanup deletes only
current objects in the exact test namespace; use an unversioned dedicated test
bucket or separately remove test versions/delete markers with an administrative
process and narrowly scoped permissions.

## Read and performance model

A complete bounded read costs `HeadObject` plus `GetObject If-Match`. This is
intentional: the HEAD rejects oversized objects before a body request, and the
conditional GET prevents a metadata/body version mixture. Nonempty ranges use
one exact HTTP range and require `206`, canonical `Content-Range`, matching
length, ETag, and exact body bytes. Empty ranges use HEAD for presence and EOF
validation.

Version 0.1 is correctness-first:

- raw partial writes read and replace one complete bounded payload;
- block-split reads fetch only the immutable radix-page paths and sparse 32 KiB
  payloads intersecting the positioned range, reusing the active path;
- block-split mutations use bounded two-pass traversal and child-before-parent
  immutable page creation; suffix pruning detaches whole discarded subtrees;
- there is no multipart upload, caching, read-ahead, coalescing, packed blocks,
  request pool, or transfer manager;
- each request incurs normal S3 latency and request charges.

Benchmarking and any bounded parallelism, cache, coalescing, or packing design
require a later proposal that preserves exact failure and publication semantics.

## Offline and live qualification

Normal tests use captured SDK HTTP requests and require no credentials. Live AWS
qualification is opt-in and never provisions a bucket:

```text
W9PT_S3_TEST_BUCKET=team-w9pt-test-bucket \
W9PT_S3_TEST_REGION=us-east-1 \
W9PT_S3_TEST_PREFIX=team/w9pt-s3-test-0123456789abcdef \
W9PT_S3_TEST_EXPECTED_OWNER=012345678901 \
W9PT_S3_TEST_CLEANUP=1 \
W9PT_S3_TEST_REQUIRED=1 \
cargo test -p w9pt-fs-storage-s3 --all-features \
  --test s3_conformance
```

The prefix must be caller-supplied, relative, and end in a test marker with a
unique run ID. Cleanup additionally requires a test-marked bucket and expected
owner. Required mode fails when bucket, region, or prefix is absent.
Qualification constructs two clients independently, first establishes their
writable guarantees, then runs target concurrency plus raw/paged-block-split
repository reopen/write/truncate/publication checks across leaf and branch boundaries.

CI also starts the digest-pinned SeaweedFS 4.42 artifact, verifies the service
identity, and requires the two-client writable target probes to pass against its
ephemeral bucket. Only after those probes, a private external-test wrapper runs
the bounded Raw/paged-BlockSplit lifecycle, all four Identity/LZ4 and None/SIV
representation combinations, and the standalone publication-boundary matrix
through the real adapter. A separate composed test commits a generated wrapped
DEK in PostgreSQL, prepares encrypted sparse content, reopens it through an
independent client, and proves that rewrap leaves all S3 objects unchanged. The
ordinary targets remain unqualified before and
after this work. Passing emulator probes and repository composition is recorded
only as behavioral evidence: the compatible-provider constructor still fails
closed until the profile has production durability, restart, multi-node, TLS,
lifecycle, and fault-test evidence.
