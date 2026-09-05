# TCP 9P → SeaweedFS integration application

This standalone development application demonstrates the narrow `w9pt`
embedding boundary:

```text
TCP 9P frames
  -> w9pt::Session
  -> Effect::Policy / Effect::Filesystem
  -> application-owned handler
  -> SeaweedFS S3 API
  -> Completion
  -> w9pt::Session
  -> TCP 9P response frames
```

It is intentionally not a production filesystem. The application keeps a
single root directory, names, object handles, and open handles in memory and
implements only unauthenticated attach, walk, open, create, positioned read,
positioned write, and release. All other operations return
`EOPNOTSUPP` and are not advertised.

File bytes are persisted as private S3 objects. Namespace and handle state are
discarded when the process exits. This fixture tests forwarding and application
ownership; it does not replace `w9pt-fs`, PostgreSQL metadata, recovery,
concurrent-writer fencing, or production S3 qualification.

## Automated integration tests

The runner uses Docker Compose to start digest-pinned SeaweedFS 4.42 and
PostgreSQL 18.6. It verifies the SeaweedFS identity, provisions an ephemeral S3
bucket, runs the S3-compatible target probes, then uses a private post-probe
test wrapper to execute the complete Raw/BlockSplit repository and standalone
publication-boundary matrix. It then runs the PostgreSQL
migration/adapter/conformance tests and a raw 9P client over TCP through this
application:

```text
./test/run-integration.sh
```

The reusable Compose definition is [compose.yaml](compose.yaml). Override the
host ports with `W9PT_SEAWEEDFS_PORT` and `W9PT_POSTGRES_PORT` when `18333` or
`15432` is unavailable.

The PostgreSQL container is exercised by `w9pt-fs-state-postgres` migration,
validation, lease, transaction, recovery, and state-store conformance tests. The
minimal TCP forwarding application deliberately retains its namespace in memory;
it does not claim that the unfinished semantic engine has integrated PostgreSQL.

The repository matrix uses two independently constructed S3 clients. It covers
Raw overwrite/gap/truncate behavior, four-logical-block sparse BlockSplit
boundaries, immutable reuse, independent reopen, abandoned preparation,
discarded publication results, and deterministic stale-CAS/reprepare ordering.
The wrapper exists only in the external integration test. Ordinary SeaweedFS
targets remain unqualified and the compatible profile remains unsupported.
Passing this single-node tmpfs suite is behavioral evidence only; it does not
establish durable acknowledgement, restart recovery, response-loss behavior,
multi-node consistency, verified TLS/SigV4 deployment, lifecycle safety, or
production support.

The test negotiates `9P2000.L`, attaches, creates `hello.txt`, writes content,
clunks, attaches again, walks and opens the file, reads it through 9P, and then
verifies the same bytes directly through S3.

Without the runner, start SeaweedFS yourself and run:

```text
W9PT_S3_COMPAT_REPOSITORY_TEST_REQUIRED=1 \
W9PT_S3_COMPAT_TEST_ENDPOINT=http://127.0.0.1:18333 \
W9PT_S3_COMPAT_TEST_BUCKET=w9pt-test-bucket \
W9PT_S3_COMPAT_TEST_PROVIDER=SeaweedFS \
W9PT_S3_COMPAT_TEST_VERSION=4.42 \
W9PT_S3_COMPAT_REPOSITORY_TEST_PREFIX=compat/repository/w9pt-s3-test-fedcba9876543210 \
cargo test -p w9pt-fs-storage-s3 --test s3_conformance \
  live_pinned_compatible_provider_repository_matrix_remains_unqualified \
  --locked -- --exact
```

The TCP fixture remains a separate direct-SDK forwarding test:

```text
W9PT_TCP_SEAWEED_TEST_REQUIRED=1 \
W9PT_TEST_S3_ENDPOINT=http://127.0.0.1:18333 \
W9PT_TEST_S3_BUCKET=w9pt-test-bucket \
W9PT_TEST_S3_PREFIX=tcp/w9pt-s3-test-development \
cargo test --manifest-path test/Cargo.toml --locked --test tcp_seaweedfs -- --nocapture
```

## Standalone server

With SeaweedFS already running:

```text
W9PT_TEST_LISTEN=127.0.0.1:5640 \
W9PT_TEST_S3_ENDPOINT=http://127.0.0.1:18333 \
W9PT_TEST_S3_BUCKET=w9pt-test-bucket \
W9PT_TEST_S3_PREFIX=tcp-server/w9pt-s3-test-development \
cargo run --manifest-path test/Cargo.toml --locked
```
