# 9P transport → SeaweedFS integration applications

This standalone development workspace demonstrates the narrow `w9pt`
embedding boundary through two application-owned transport profiles:

```text
TCP byte stream                         HTTP GET /9p
  -> Session::receive_bytes               -> WebSocket upgrade (subprotocol 9p)
                                            -> Session::receive_frame
                 \                       /
                  -> Effect::Policy / Effect::Filesystem
                  -> application-owned handler
                  -> SeaweedFS S3 API
                  -> Completion
                  -> Session
                 /                       \
       TCP response bytes       one binary WebSocket message per response frame
```

The WebSocket server also exposes `GET /healthz`, which returns `204 No
Content`. `/9p` requires `Sec-WebSocket-Protocol: 9p`; each binary message must
contain exactly one complete 9P frame, including its four-byte length. Text and
malformed binary inputs receive WebSocket close codes 1003 and 1002,
respectively. WebSocket messages and frames are bounded to 1 MiB.

These applications are intentionally not production filesystems. They keep a
single root directory, names, object handles, and open handles in memory and
implement only unauthenticated attach, walk, open, create, positioned read,
positioned write, and release. All other operations return `EOPNOTSUPP` and are
not advertised.

File bytes are persisted as private S3 objects. Namespace, handle, and session
state are discarded when the process exits. These fixtures test transport
forwarding and application ownership; they do not replace `w9pt-fs`,
PostgreSQL metadata, recovery, concurrent-writer fencing, a durable transport
gateway, or production S3 qualification. The HTTP server has no TLS, Origin or
proxy policy, authentication, reconnection, or session migration support.

## Automated integration tests

The runner uses Docker Compose to start digest-pinned SeaweedFS 4.42 and
PostgreSQL 18.6. It verifies the SeaweedFS identity, provisions an ephemeral S3
bucket, runs the S3-compatible target probes, then uses a private post-probe
test wrapper to execute the complete Raw/paged-BlockSplit repository, all four
Identity/LZ4 and None/SIV representation combinations, and the standalone
publication-boundary matrix. It then runs the PostgreSQL
migration/adapter/conformance tests and the same 9P lifecycle through both the
TCP stream and HTTP/WebSocket message profiles:

```text
./test/run-integration.sh
```

The reusable Compose definition is [compose.yaml](compose.yaml). Override the
host ports with `W9PT_SEAWEEDFS_PORT` and `W9PT_POSTGRES_PORT` when `18333` or
`15432` is unavailable.

The PostgreSQL container is exercised by `w9pt-fs-state-postgres` migration,
validation, lease, transaction, recovery, and state-store conformance tests. The
minimal forwarding applications deliberately retain their namespace in memory;
it does not claim that the unfinished semantic engine has integrated PostgreSQL.

The bounded composition test uses its own private post-probe target wrapper in
`tests/support/content_target.rs`. It atomically commits an encrypted file and
generated wrapped DEK in PostgreSQL, discards a different retry candidate,
prepares sparse branch-crossing content in SeaweedFS, reopens it from a separate
database and S3 client with no key cache, and rewraps the same DEK under a new
public test master. It compares every object key and byte before and after
rewrap. Ordinary SeaweedFS targets remain unqualified throughout.

The repository matrix uses two independently constructed S3 clients. It covers
Raw overwrite/gap/truncate behavior, sparse BlockSplit operations across the
127/128 leaf and 16383/16384 branch boundaries, root growth/collapse,
shrink/re-extension, immutable reuse, independent reopen, abandoned preparation,
discarded publication results, and deterministic stale-CAS/reprepare ordering.
The wrapper exists only in the external integration test. Ordinary SeaweedFS
targets remain unqualified and the compatible profile remains unsupported.
Passing this single-node tmpfs suite is behavioral evidence only; it does not
establish durable acknowledgement, restart recovery, response-loss behavior,
multi-node consistency, verified TLS/SigV4 deployment, lifecycle safety, or
production support.

Both profiles negotiate `9P2000.L`, attach, create a file, write content, clunk,
attach again, walk and open the file, read it through 9P, and then verify the
same bytes directly through S3. The WebSocket profile additionally verifies
HTTP health, subprotocol negotiation and rejection, independent upgraded
sessions, text rejection, and malformed-frame rejection.

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

The HTTP/WebSocket fixture uses a separate private prefix:

```text
W9PT_WEBSOCKET_SEAWEED_TEST_REQUIRED=1 \
W9PT_TEST_S3_ENDPOINT=http://127.0.0.1:18333 \
W9PT_TEST_S3_BUCKET=w9pt-test-bucket \
W9PT_TEST_WEBSOCKET_S3_PREFIX=websocket/w9pt-s3-test-development \
cargo test --manifest-path test/Cargo.toml --locked --test websocket_seaweedfs -- --nocapture
```

## Standalone servers

With SeaweedFS already running:

```text
W9PT_TEST_LISTEN=127.0.0.1:5640 \
W9PT_TEST_S3_ENDPOINT=http://127.0.0.1:18333 \
W9PT_TEST_S3_BUCKET=w9pt-test-bucket \
W9PT_TEST_S3_PREFIX=tcp-server/w9pt-s3-test-development \
cargo run --manifest-path test/Cargo.toml --locked --bin w9pt-tcp-seaweedfs-test
```

Run the HTTP/WebSocket profile on `http://127.0.0.1:8080`; its WebSocket URL is
`ws://127.0.0.1:8080/9p`:

```text
W9PT_TEST_WEBSOCKET_LISTEN=127.0.0.1:8080 \
W9PT_TEST_S3_ENDPOINT=http://127.0.0.1:18333 \
W9PT_TEST_S3_BUCKET=w9pt-test-bucket \
W9PT_TEST_S3_PREFIX=websocket-server/w9pt-s3-test-development \
cargo run --manifest-path test/Cargo.toml --locked --bin w9pt-websocket-seaweedfs-test
```
