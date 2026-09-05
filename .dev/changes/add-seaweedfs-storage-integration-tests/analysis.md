# Analysis: SeaweedFS Storage Integration Tests

## Existing Coverage

- `crates/w9pt-fs-storage/tests/block_split_model.rs` compares deterministic
  BlockSplit operation traces with a byte-vector model across block boundaries.
- `crates/w9pt-fs-storage/src/layout/block_split.rs` has focused tests for
  sparse holes, zero-padded final blocks, full-block overwrite without old-data
  reads, verified partial-block read-modify-write, shrink tail clearing, limits,
  and corruption.
- `crates/w9pt-fs-storage/src/testing/repository_conformance.rs` exercises both
  Raw and BlockSplit through two independently held target clients, including
  create, standalone head publication, reopen, positioned write, preparation
  loss, truncate, and re-extension.
- `crates/w9pt-fs-storage-s3/tests/s3_conformance.rs` runs full repository
  conformance only after live Amazon S3 qualification.
- The SeaweedFS test currently calls `S3Target::probe_pair`, asserts the targets
  remain unqualified, and confirms the compatible profile is unsupported. It
  does not construct a repository.
- `test/tests/tcp_seaweedfs.rs` proves 9P framing/effect forwarding through an
  application that directly performs whole-object AWS SDK operations. It does
  not use `S3Target`, `ContentRepository`, Raw manifests, or BlockSplit.

## Exact Evidence Gap

`ContentRepository::new` correctly rejects any target that does not advertise
all required writable guarantees. An unqualified SeaweedFS `S3Target`
advertises `TargetGuarantees::NONE`; calling `qualify_pair` is both semantically
incorrect and blocked by the Amazon endpoint validator. Therefore the current
SeaweedFS compatibility probe cannot reach repository code.

The existing generic repository conformance also uses only small `abc`/`XYZ`
payloads. Although it selects BlockSplit, that branch stays inside one 32 KiB
block and cannot expose incorrect multi-object behavior in an S3-compatible
provider.

## Architecture Decision

Define the guarantee override only as a private newtype inside the external S3
integration-test module:

```text
unqualified S3Target pair
  -> S3Target::probe_pair
  -> private CompatibilityProbeTarget wrappers
  -> ContentRepository Raw/BlockSplit tests
  -> assert original production qualification remains closed
```

The wrapper delegates all object operations to the real `S3Target` and reports
the required guarantees only to repositories owned by that test. It must have
no public constructor, no production export, no Cargo feature, and no path into
application code. This is preferable to an unchecked repository constructor or
a core test helper that could be called by non-test consumers.

Reusable repository scenarios may be refactored or expanded in
`w9pt-fs-storage::testing`, but those helpers continue to require a target that
advertises writable guarantees. The SeaweedFS-specific wrapper remains outside
the backend-neutral crate.

## Scenario Selection

## Frozen Integration Contract

- Provider artifact:
  `chrislusf/seaweedfs@sha256:f7cbc8bdbbf60a1aaba7d61784a3bdff3ec1e0657f6ad0b26d5b6ab2cd9d0dc6`.
- Required identity header: `Server: SeaweedFS 30GB 4.42`.
- Shared routing variables: `W9PT_S3_COMPAT_TEST_ENDPOINT`,
  `W9PT_S3_COMPAT_TEST_BUCKET`, `W9PT_S3_COMPAT_TEST_PROVIDER`, and
  `W9PT_S3_COMPAT_TEST_VERSION`.
- Target-probe gate/root: `W9PT_S3_COMPAT_TEST_REQUIRED=1` and
  `W9PT_S3_COMPAT_TEST_PREFIX`.
- Repository gate/root: `W9PT_S3_COMPAT_REPOSITORY_TEST_REQUIRED=1` and
  `W9PT_S3_COMPAT_REPOSITORY_TEST_PREFIX`.
- Each root is a checked `S3QualificationNamespace`; repository scenarios append
  distinct `raw`, `block-split`, `publication`, and `concurrency` children and
  use non-overlapping stable file/mutation IDs.
- Live logical models contain at most four 32 KiB version-1 blocks plus 257
  bytes. Individual writes are bounded to one full block or one small
  cross-boundary/gap mutation; the existing repository/adapter hard limits
  remain authoritative.
- Readiness is bounded by 30 one-second probes, while Compose startup also has a
  60-second wait bound. Failure output is limited to the integration project's
  service logs, and teardown targets only that unique Compose project.

### Raw

- Empty and nonempty creation and second-client reopen.
- Positioned unaligned overwrite.
- Write beyond EOF with a zero-filled logical gap.
- Shrink and later re-extension without resurrected bytes.
- Publication and immediate independent-client visibility after each version.

### BlockSplit

- Initial content spanning multiple 32 KiB blocks.
- Exact reads within and across boundaries and at logical EOF.
- Partial write crossing two blocks, forcing verified read-modify-write.
- Exact full-block overwrite, which must not depend on old payload reads.
- Sparse write beyond one or more missing logical blocks.
- All-zero blocks represented as holes.
- Shrink inside the final retained block and re-extension with a zeroed tail.
- Reopen under a repository whose creation default changed to Raw, proving the
  persisted method remains authoritative.

### Publication boundaries

- Prepare immutable payloads and a manifest, discard the preparation before
  head publication, reconstruct through the other client, and observe the old
  complete published version.
- Publish a prepared version, discard the returned publication value, reopen
  through the other client, and observe the complete new version.
- Prepare two updates from one base, publish one, require the stale publication
  to conflict, reread/reprepare against the winner, publish again, and observe a
  valid serial result without mixed blocks.

These boundaries are deterministic and meaningful with a single-node tmpfs
provider. Container kills, network cuts, and restart checks would be timing
sensitive and still would not prove durable acknowledgement; those are excluded.

## Affected Files

| File | Proposed change |
| --- | --- |
| `crates/w9pt-fs-storage/src/testing/repository_conformance.rs` | Expose/refactor bounded per-method scenarios and add richer Raw/BlockSplit lifecycle and controlled-publication checks that remain backend-neutral. |
| `crates/w9pt-fs-storage/tests/object_store_conformance.rs` | Verify the expanded helpers against independent memory clients and retain the guarantee gate. |
| `crates/w9pt-fs-storage-s3/tests/s3_conformance.rs` | Add the private compatibility wrapper, independent Seaweed clients, live repository cases, and before/after unsupported-profile assertions. Split a dedicated Seaweed test module only if shared environment/client code first becomes reusable. |
| `test/run-integration.sh` | Run the new repository test in required mode and emit Seaweed logs for any failed integration phase. |
| `.github/workflows/s3-target.yml` | Add an explicit required repository-integration step against the pinned Compose artifact. |
| `test/README.md` | Describe the repository layer now exercised and retain the TCP/non-production distinction. |
| `crates/w9pt-fs-storage-s3/README.md` | Record Raw/BlockSplit Seaweed behavioral evidence and its qualification limits. |
| `README.md` and `.dev/project.md` | Update project-level testing status without claiming compatible-provider support. |
| `test/compose.yaml` | No change expected unless implementation discovers a missing bounded health/readiness facility. |

No production file under `crates/w9pt-fs-storage-s3/src/` should need semantic
changes. Any implementation pressure to weaken target guarantees or endpoint
qualification is a stop condition.

## Conventions to Preserve

- Exact image digest plus reported provider/version verification.
- Caller-owned SDK clients and path-style endpoint configuration in tests.
- Unique private repository prefixes; no visible filesystem paths as keys.
- Checked arithmetic and bounded payloads, requests, retries, waits, and logs.
- Two independently constructed clients rather than two clones of one client.
- One terminal result for each conditional operation and no hidden mutation
  retry.
- Immutable payload and manifest publication before standalone test head CAS.
- No dependency from core crates to the AWS SDK, Tokio, Docker, or SeaweedFS.

## Risks and Dependencies

- The provider may pass behavior probes while lacking production durability,
  authentication, lifecycle, or multi-node guarantees. Tests must state this
  explicitly.
- More object requests increase CI duration and can expose eventual provider
  bugs. Keep traces deterministic and bounded to a few blocks.
- A fixed bucket with parallel cases can contaminate results. Use scenario child
  prefixes and avoid shared head keys.
- True response-loss and after-commit ambiguity cannot be scheduled reliably
  through Compose; retained SDK replay tests remain authoritative.
- The approved S3 target and storage-method changes are prerequisites. No
  `.dev/specs` baseline exists yet, so this proposal adds a standalone delta
  domain rather than modifying an archived requirement.
